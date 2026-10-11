use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::END;
use crate::error::{BuildError, ValidationIssue};
use crate::node::{Node, Router};

/// Declares a workflow graph. Call [`build`](Self::build) to validate it.
///
/// Methods record declarations and never fail; every problem is reported
/// together by `build`.
#[derive(Default)]
pub struct WorkflowBuilder {
    nodes: Vec<(String, Arc<dyn Node>)>,
    entry: Option<String>,
    edges: Vec<(String, String)>,
    routers: Vec<RouterDecl>,
    joins: Vec<(Vec<String>, String)>,
}

struct RouterDecl {
    from: String,
    router: Arc<dyn Router>,
    routes: Vec<(String, String)>,
}

impl WorkflowBuilder {
    /// Creates an empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node. Declaration order determines merge and trace order.
    pub fn add_node(&mut self, name: impl Into<String>, node: impl Node) -> &mut Self {
        self.nodes.push((name.into(), Arc::new(node)));
        self
    }

    /// Sets the node that runs in the first step.
    pub fn set_entry(&mut self, name: impl Into<String>) -> &mut Self {
        self.entry = Some(name.into());
        self
    }

    /// Schedules `to` (a node or [`END`]) in the step after `from` completes.
    ///
    /// A node with several static edges fans out: all targets run
    /// concurrently in the next step.
    pub fn add_edge(&mut self, from: impl Into<String>, to: impl Into<String>) -> &mut Self {
        self.edges.push((from.into(), to.into()));
        self
    }

    /// After `from` completes, runs `router` and schedules the target mapped
    /// to the label it returns. `routes` maps labels to node names or [`END`].
    pub fn add_router<I, L, T>(
        &mut self,
        from: impl Into<String>,
        router: impl Router,
        routes: I,
    ) -> &mut Self
    where
        I: IntoIterator<Item = (L, T)>,
        L: Into<String>,
        T: Into<String>,
    {
        self.routers.push(RouterDecl {
            from: from.into(),
            router: Arc::new(router),
            routes: routes
                .into_iter()
                .map(|(l, t)| (l.into(), t.into()))
                .collect(),
        });
        self
    }

    /// Schedules `to` once every node in `sources` has completed since `to`
    /// last ran. Sources may finish in different steps.
    pub fn add_join<I, S>(&mut self, sources: I, to: impl Into<String>) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.joins
            .push((sources.into_iter().map(Into::into).collect(), to.into()));
        self
    }

    /// Validates the declarations and produces a runnable [`Workflow`].
    ///
    /// The builder is left untouched, so it can be extended and built again.
    pub fn build(&self) -> Result<Workflow, BuildError> {
        let mut v = Validator {
            issues: Vec::new(),
            index: HashMap::new(),
        };
        let n = self.nodes.len();

        if n == 0 {
            v.issues.push(ValidationIssue::NoNodes);
        }
        for (i, (name, _)) in self.nodes.iter().enumerate() {
            if name.is_empty() {
                v.issues.push(ValidationIssue::EmptyName);
            } else if name.starts_with("__") {
                v.issues.push(ValidationIssue::ReservedName(name.clone()));
            }
            if v.index.contains_key(name) {
                v.issues.push(ValidationIssue::DuplicateNode(name.clone()));
            } else {
                v.index.insert(name.clone(), i);
            }
        }

        let entry = match &self.entry {
            None if n > 0 => {
                v.issues.push(ValidationIssue::MissingEntry);
                None
            }
            None => None,
            Some(name) => v.node(name, || "entry".to_string()),
        };

        let mut static_out: Vec<Vec<Target>> = vec![Vec::new(); n];
        // Tracks declared edges even when their target is unknown, so a typo
        // in a target is not also reported as a dead end.
        let mut has_static_edge = vec![false; n];
        for (from, to) in &self.edges {
            let ctx = || format!("edge `{from}` -> `{to}`");
            let (f, t) = (v.node(from, ctx), v.target(to, ctx));
            if let Some(f) = f {
                has_static_edge[f] = true;
            }
            if let (Some(f), Some(t)) = (f, t)
                && !static_out[f].contains(&t)
            {
                static_out[f].push(t);
            }
        }

        let mut routed: Vec<Option<RouteTable>> = vec![None; n];
        for decl in &self.routers {
            let from = &decl.from;
            let f = v.node(from, || format!("router on `{from}`"));
            if decl.routes.is_empty() {
                v.issues.push(ValidationIssue::EmptyRoutes(from.clone()));
            }
            let mut seen = HashSet::new();
            let mut routes = Vec::new();
            for (label, to) in &decl.routes {
                if !seen.insert(label) {
                    v.issues.push(ValidationIssue::DuplicateRouteLabel {
                        node: from.clone(),
                        label: label.clone(),
                    });
                }
                if let Some(t) = v.target(to, || format!("route `{label}` of router on `{from}`")) {
                    routes.push((label.clone(), t));
                }
            }
            if let Some(f) = f {
                if routed[f].is_some() {
                    v.issues
                        .push(ValidationIssue::MultipleRouters(from.clone()));
                } else {
                    routed[f] = Some((decl.router.clone(), routes));
                }
            }
        }

        let mut joins = Vec::new();
        for (sources, to) in &self.joins {
            let target = v.node(to, || format!("join into `{to}`"));
            if sources.is_empty() {
                v.issues.push(ValidationIssue::EmptyJoin(to.clone()));
            }
            let mut ids = Vec::new();
            for s in sources {
                if let Some(id) = v.node(s, || format!("join into `{to}`"))
                    && !ids.contains(&id)
                {
                    ids.push(id);
                }
            }
            if let Some(target) = target {
                joins.push(Join {
                    sources: ids,
                    target,
                });
            }
        }

        let mut feeds_joins: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (j, join) in joins.iter().enumerate() {
            for &s in &join.sources {
                feeds_joins[s].push(j);
            }
        }

        // Structural checks that need the resolved graph.
        let mut inbound = vec![0usize; n];
        if let Some(e) = entry {
            inbound[e] += 1;
        }
        let targets_of = |id: usize| -> Vec<Target> {
            let mut ts = static_out[id].clone();
            if let Some((_, routes)) = &routed[id] {
                ts.extend(routes.iter().map(|(_, t)| *t));
            }
            ts
        };
        for id in 0..n {
            for t in targets_of(id) {
                if let Target::Node(t) = t {
                    inbound[t] += 1;
                }
            }
        }
        let mut join_inbound = vec![0usize; n];
        for join in &joins {
            join_inbound[join.target] += 1;
        }

        for (id, (name, _)) in self.nodes.iter().enumerate() {
            if v.index.get(name) != Some(&id) {
                continue; // duplicate; already reported
            }
            if routed[id].is_some() && has_static_edge[id] {
                v.issues.push(ValidationIssue::MixedEdges(name.clone()));
            }
            if routed[id].is_none() && !has_static_edge[id] && feeds_joins[id].is_empty() {
                v.issues.push(ValidationIssue::DeadEnd(name.clone()));
            }
            if join_inbound[id] > 1 || (join_inbound[id] == 1 && inbound[id] > 0) {
                v.issues
                    .push(ValidationIssue::JoinTargetHasOtherInbound(name.clone()));
            }
        }

        if let Some(e) = entry {
            let mut reached = vec![false; n];
            let mut queue = VecDeque::from([e]);
            reached[e] = true;
            while let Some(id) = queue.pop_front() {
                let join_targets = feeds_joins[id]
                    .iter()
                    .map(|&j| Target::Node(joins[j].target));
                for t in targets_of(id).into_iter().chain(join_targets) {
                    if let Target::Node(t) = t
                        && !reached[t]
                    {
                        reached[t] = true;
                        queue.push_back(t);
                    }
                }
            }
            for (id, (name, _)) in self.nodes.iter().enumerate() {
                if !reached[id] && v.index.get(name) == Some(&id) {
                    v.issues.push(ValidationIssue::Unreachable(name.clone()));
                }
            }
        }

        if !v.issues.is_empty() {
            return Err(BuildError { issues: v.issues });
        }

        let outgoing = static_out
            .into_iter()
            .zip(routed)
            .map(|(targets, routed)| match routed {
                Some((router, routes)) => Outgoing::Routed { router, routes },
                None => Outgoing::Static(targets),
            })
            .collect();

        Ok(Workflow {
            graph: Arc::new(Graph {
                names: self.nodes.iter().map(|(name, _)| name.clone()).collect(),
                nodes: self.nodes.iter().map(|(_, node)| node.clone()).collect(),
                entry: entry.expect("entry is validated"),
                outgoing,
                joins,
                feeds_joins,
            }),
        })
    }
}

type RouteTable = (Arc<dyn Router>, Vec<(String, Target)>);

struct Validator {
    issues: Vec<ValidationIssue>,
    index: HashMap<String, usize>,
}

impl Validator {
    fn node(&mut self, name: &str, context: impl FnOnce() -> String) -> Option<usize> {
        let found = self.index.get(name).copied();
        if found.is_none() {
            self.issues.push(ValidationIssue::UnknownNode {
                node: name.to_string(),
                context: context(),
            });
        }
        found
    }

    fn target(&mut self, name: &str, context: impl FnOnce() -> String) -> Option<Target> {
        if name == END {
            Some(Target::End)
        } else {
            self.node(name, context).map(Target::Node)
        }
    }
}

/// A validated, immutable workflow. Cloning is cheap and clones share the graph,
/// so one workflow can serve many concurrent runs.
#[derive(Clone)]
pub struct Workflow {
    pub(crate) graph: Arc<Graph>,
}

impl Workflow {
    /// Node names in declaration order.
    pub fn node_names(&self) -> impl Iterator<Item = &str> {
        self.graph.names.iter().map(String::as_str)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Node(usize),
    End,
}

pub(crate) enum Outgoing {
    Static(Vec<Target>),
    Routed {
        router: Arc<dyn Router>,
        routes: Vec<(String, Target)>,
    },
}

pub(crate) struct Join {
    pub sources: Vec<usize>,
    pub target: usize,
}

pub(crate) struct Graph {
    pub names: Vec<String>,
    pub nodes: Vec<Arc<dyn Node>>,
    pub entry: usize,
    pub outgoing: Vec<Outgoing>,
    pub joins: Vec<Join>,
    /// For each node, the joins it is a source of.
    pub feeds_joins: Vec<Vec<usize>>,
}
