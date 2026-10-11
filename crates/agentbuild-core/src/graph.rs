use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::END;
use crate::error::{BuildError, ValidationIssue};
use crate::node::{Node, Router};
use crate::policy::NodePolicy;
use crate::reducer::Reducer;

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
    gotos: Vec<(String, Vec<String>)>,
    reducers: Vec<(String, Reducer)>,
    policies: Vec<(String, NodePolicy)>,
}

// Nodes and routers are user closures, so Debug shows names only.
impl std::fmt::Debug for WorkflowBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowBuilder")
            .field(
                "nodes",
                &self.nodes.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .field("entry", &self.entry)
            .finish_non_exhaustive()
    }
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

    /// Declares the targets `node` may jump to by returning
    /// [`NodeOutput::Command`](crate::NodeOutput::Command). Targets are node
    /// names or [`END`]; they are scheduled in addition to static edges.
    ///
    /// A node with `may_goto` targets and no static edges or joins must name at
    /// least one target every time it runs.
    pub fn may_goto<I, S>(&mut self, node: impl Into<String>, targets: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.gotos
            .push((node.into(), targets.into_iter().map(Into::into).collect()));
        self
    }

    /// Declares how writes to the state key `key` are combined. Keys without a
    /// declaration use [`Reducer::Replace`].
    pub fn reducer(&mut self, key: impl Into<String>, reducer: Reducer) -> &mut Self {
        self.reducers.push((key.into(), reducer));
        self
    }

    /// Sets how `node` is executed: per-attempt timeout and retries. Nodes
    /// without a policy run once, with no time limit.
    pub fn policy(&mut self, node: impl Into<String>, policy: NodePolicy) -> &mut Self {
        self.policies.push((node.into(), policy));
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

        let mut goto: Vec<Vec<(String, Target)>> = vec![Vec::new(); n];
        for (from, targets) in &self.gotos {
            let f = v.node(from, || format!("may_goto of `{from}`"));
            for to in targets {
                let t = v.target(to, || format!("may_goto of `{from}`"));
                if let (Some(f), Some(t)) = (f, t)
                    && !goto[f].iter().any(|(name, _)| name == to)
                {
                    goto[f].push((to.clone(), t));
                }
            }
        }

        let mut reducers = HashMap::new();
        for (key, reducer) in &self.reducers {
            if reducers.insert(key.clone(), *reducer).is_some() {
                v.issues
                    .push(ValidationIssue::DuplicateReducer(key.clone()));
            }
        }

        let mut policies = vec![NodePolicy::default(); n];
        let mut has_policy = HashSet::new();
        for (node, policy) in &self.policies {
            if !has_policy.insert(node) {
                v.issues
                    .push(ValidationIssue::DuplicatePolicy(node.clone()));
                continue;
            }
            if let Err(reason) = policy.validate() {
                v.issues.push(ValidationIssue::InvalidPolicy {
                    node: node.clone(),
                    reason,
                });
            }
            if let Some(id) = v.node(node, || format!("policy for `{node}`")) {
                policies[id] = policy.clone();
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
            ts.extend(goto[id].iter().map(|(_, t)| *t));
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
            if routed[id].is_some() && !goto[id].is_empty() {
                v.issues.push(ValidationIssue::GotoWithRouter(name.clone()));
            }
            if routed[id].is_none()
                && !has_static_edge[id]
                && feeds_joins[id].is_empty()
                && goto[id].is_empty()
            {
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

        let mut graph = Graph {
            names: self.nodes.iter().map(|(name, _)| name.clone()).collect(),
            nodes: self.nodes.iter().map(|(_, node)| node.clone()).collect(),
            entry: entry.expect("entry is validated"),
            outgoing,
            goto,
            joins,
            feeds_joins,
            reducers,
            policies,
            fingerprint: String::new(),
        };
        graph.fingerprint = fingerprint(&graph);
        Ok(Workflow {
            graph: Arc::new(graph),
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

impl std::fmt::Debug for Workflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workflow")
            .field("nodes", &self.graph.names)
            .field("entry", &self.graph.names[self.graph.entry])
            .field("fingerprint", &self.graph.fingerprint)
            .finish_non_exhaustive()
    }
}

impl Workflow {
    /// Node names in declaration order.
    pub fn node_names(&self) -> impl Iterator<Item = &str> {
        self.graph.names.iter().map(String::as_str)
    }

    /// A stable hash of the workflow's structure: nodes, edges, routes,
    /// `may_goto` targets, joins and reducers. Node *implementations* are not
    /// included. Snapshots record it so a run is only resumed on the same graph.
    pub fn fingerprint(&self) -> &str {
        &self.graph.fingerprint
    }
}

/// FNV-1a over a canonical text form. Not cryptographic; used only to detect
/// structural changes. `DefaultHasher` is avoided because its output may change
/// between Rust releases, and fingerprints are persisted.
fn fingerprint(g: &Graph) -> String {
    use std::fmt::Write;
    let name = |t: &Target| match t {
        Target::Node(id) => g.names[*id].as_str(),
        Target::End => END,
    };
    let mut text = String::new();
    let _ = write!(text, "v1;entry={};", g.names[g.entry]);
    for (id, node) in g.names.iter().enumerate() {
        let _ = write!(text, "node={node}[");
        match &g.outgoing[id] {
            Outgoing::Static(ts) => {
                let mut ts: Vec<&str> = ts.iter().map(name).collect();
                ts.sort_unstable();
                let _ = write!(text, "edges={}", ts.join(","));
            }
            Outgoing::Routed { routes, .. } => {
                let routes: Vec<String> = routes
                    .iter()
                    .map(|(l, t)| format!("{l}>{}", name(t)))
                    .collect();
                let _ = write!(text, "routes={}", routes.join(","));
            }
        }
        let mut gotos: Vec<&str> = g.goto[id].iter().map(|(n, _)| n.as_str()).collect();
        gotos.sort_unstable();
        let _ = write!(text, ";goto={}];", gotos.join(","));
    }
    for join in &g.joins {
        let mut sources: Vec<&str> = join.sources.iter().map(|&s| g.names[s].as_str()).collect();
        sources.sort_unstable();
        let _ = write!(text, "join={}>{};", sources.join(","), g.names[join.target]);
    }
    let mut reducers: Vec<String> = g
        .reducers
        .iter()
        .map(|(k, r)| format!("{k}:{r:?}"))
        .collect();
    reducers.sort_unstable();
    let _ = write!(text, "reducers={}", reducers.join(","));

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
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
    /// For each node, the declared `may_goto` targets by name.
    pub goto: Vec<Vec<(String, Target)>>,
    pub joins: Vec<Join>,
    /// For each node, the joins it is a source of.
    pub feeds_joins: Vec<Vec<usize>>,
    pub reducers: HashMap<String, Reducer>,
    /// Per node, in declaration order.
    pub policies: Vec<NodePolicy>,
    pub fingerprint: String,
}

impl Graph {
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n == name)
    }
}
