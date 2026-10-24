//! Specialist agents: default recipes for common jobs that you configure,
//! extend or replace (design 0019).
//!
//! A [`Specialist`] is a *recipe*, not an agent: a name, a description (what
//! a coordinator reads to decide when to use it), instructions, and default
//! agent settings. [`Specialist::build`] makes the [`Agent`]. The built-in
//! ones ([`researcher`], [`summarizer`], [`classifier`], [`extractor`],
//! [`planner`], [`writer`], [`critic`], [`fact_checker`], [`code_reviewer`],
//! [`translator`], [`support_agent`]) are prompts and sensible defaults;
//! tools come from you, and no claim is made about their quality on your
//! data.
//!
//! A registry keeps recipes under their names, so workflows and
//! configuration can refer to them: [`register`], [`get`], [`available`],
//! [`unregister`]. It is process-wide and thread-safe.
//!
//! ```
//! use std::sync::Arc;
//! use agentbuild_agent::specialists;
//! use agentbuild_providers::FakeModel;
//!
//! let careful = specialists::researcher()
//!     .extend("careful-researcher")
//!     .append_instructions("Always give two sources.");
//! let agent = careful.build(Arc::new(FakeModel::new())).unwrap();
//! # let _ = agent;
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock, RwLock};

use agentbuild_providers::Model;
use serde_json::Value;

use crate::agent::{Agent, AgentBuildError, AgentBuilder, Limits};
use crate::tool::Tool;

const NAME_LIMIT: usize = 64;

/// Why a specialist could not be made, registered or found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SpecialistError {
    /// The name is empty, too long, or has characters other than letters,
    /// digits, `-` and `_`.
    #[error("specialist name {0:?}: use letters, digits, - and _ (at most 64)")]
    InvalidName(String),
    /// The description is empty. Coordinators choose by it.
    #[error("a specialist needs a description: coordinators choose by it")]
    MissingDescription,
    /// A recipe with this name is already registered.
    #[error("a specialist named {0:?} is already registered (use register_replace to overwrite)")]
    AlreadyRegistered(String),
    /// No recipe is registered under this name.
    #[error("no specialist {name:?}; registered: {registered:?}")]
    Unknown {
        /// The name that was asked for.
        name: String,
        /// The names that are registered, sorted.
        registered: Vec<String>,
    },
}

/// Makes the tools a recipe uses when it has none.
type DefaultTools = fn() -> Vec<Arc<dyn Tool>>;
type Configure = Arc<dyn Fn(AgentBuilder) -> AgentBuilder + Send + Sync>;

/// A recipe for an agent: name, description, instructions and default
/// settings. See the [module documentation](self).
#[derive(Clone)]
pub struct Specialist {
    name: String,
    description: String,
    instructions: String,
    tools: Vec<Arc<dyn Tool>>,
    output: Option<Value>,
    limits: Option<Limits>,
    default_tools: Option<DefaultTools>,
    configure: Vec<Configure>,
}

impl fmt::Debug for Specialist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Specialist")
            .field("name", &self.name)
            .field("description", &self.description)
            .field(
                "tools",
                &self
                    .tools
                    .iter()
                    .map(|t| &t.spec().name)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Specialist {
    /// A recipe. `name` uses letters, digits, `-` and `_` (at most 64);
    /// `description` says when to use it.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Result<Self, SpecialistError> {
        let name = name.into();
        let description = description.into();
        let valid = !name.is_empty()
            && name.chars().count() <= NAME_LIMIT
            && name
                .chars()
                .all(|c| c == '-' || c == '_' || c.is_alphanumeric());
        if !valid {
            return Err(SpecialistError::InvalidName(name));
        }
        if description.trim().is_empty() {
            return Err(SpecialistError::MissingDescription);
        }
        Ok(Self {
            name,
            description,
            instructions: instructions.into(),
            tools: Vec::new(),
            output: None,
            limits: None,
            default_tools: None,
            configure: Vec::new(),
        })
    }

    /// The name it is registered and referred to by.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What a coordinator reads to decide when to use it.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The system prompt.
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// The names of the tools the recipe carries (not counting
    /// [`default_tools`](Self::default_tools)).
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.spec().name.as_str()).collect()
    }

    /// Adds a tool.
    pub fn tool(self, tool: impl Tool) -> Self {
        self.shared_tool(Arc::new(tool))
    }

    /// Adds a shared tool.
    pub fn shared_tool(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// The agent must end with JSON matching `schema`
    /// ([`AgentBuilder::output`]).
    pub fn output(mut self, schema: Value) -> Self {
        self.output = Some(schema);
        self
    }

    /// Agent limits.
    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Tools to use when the recipe has none: a function called at build
    /// time (the researcher uses it for web search when a key is set).
    pub fn default_tools(mut self, make: fn() -> Vec<Arc<dyn Tool>>) -> Self {
        self.default_tools = Some(make);
        self
    }

    /// Any other agent setting (approval, hooks, storage, memory,
    /// fallbacks ...): `f` receives the builder when the agent is built.
    pub fn configure(
        mut self,
        f: impl Fn(AgentBuilder) -> AgentBuilder + Send + Sync + 'static,
    ) -> Self {
        self.configure.push(Arc::new(f));
        self
    }

    /// Replaces the instructions.
    pub fn instructions_replaced(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = instructions.into();
        self
    }

    /// Appends a paragraph to the instructions.
    pub fn append_instructions(mut self, add: &str) -> Self {
        self.instructions = format!("{}\n\n{}", self.instructions.trim_end(), add.trim());
        self
    }

    /// Replaces the description.
    pub fn described(mut self, description: impl Into<String>) -> Result<Self, SpecialistError> {
        let description = description.into();
        if description.trim().is_empty() {
            return Err(SpecialistError::MissingDescription);
        }
        self.description = description;
        Ok(self)
    }

    /// A new recipe from this one, under `name`. Change it with
    /// [`append_instructions`](Self::append_instructions),
    /// [`instructions_replaced`](Self::instructions_replaced),
    /// [`tool`](Self::tool) (adds to the existing tools), and so on. This
    /// recipe is unchanged.
    pub fn extend(&self, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..self.clone()
        }
    }

    /// The agent, around `model`.
    pub fn build(&self, model: Arc<dyn Model>) -> Result<Agent, AgentBuildError> {
        self.build_with(model, |builder| builder)
    }

    /// [`build`](Self::build), with `overrides` applied to the builder after
    /// the recipe (so `.instructions(..)` or `.limits(..)` there win).
    pub fn build_with(
        &self,
        model: Arc<dyn Model>,
        overrides: impl FnOnce(AgentBuilder) -> AgentBuilder,
    ) -> Result<Agent, AgentBuildError> {
        let mut builder = Agent::builder_shared(model).instructions(self.instructions.clone());
        let mut tools = self.tools.clone();
        if tools.is_empty()
            && let Some(make) = self.default_tools
        {
            tools = make();
        }
        for tool in tools {
            builder = builder.shared_tool(tool);
        }
        if let Some(schema) = &self.output {
            builder = builder.output(schema.clone());
        }
        if let Some(limits) = &self.limits {
            builder = builder.limits(limits.clone());
        }
        for configure in &self.configure {
            builder = configure(builder);
        }
        overrides(builder).build()
    }
}

// ------------------------------------------------------------------ registry

type Registry = RwLock<BTreeMap<String, Specialist>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let builtin = [
            researcher(),
            summarizer(),
            classifier(),
            extractor(),
            planner(),
            writer(),
            critic(),
            fact_checker(),
            code_reviewer(),
            translator(),
            support_agent(),
        ];
        RwLock::new(
            builtin
                .into_iter()
                .map(|s| (s.name.clone(), s))
                .collect::<BTreeMap<_, _>>(),
        )
    })
}

/// Keeps a recipe under its name. Registering a taken name fails; use
/// [`register_replace`] to overwrite.
pub fn register(specialist: Specialist) -> Result<(), SpecialistError> {
    let mut map = registry().write().unwrap_or_else(|e| e.into_inner());
    if map.contains_key(&specialist.name) {
        return Err(SpecialistError::AlreadyRegistered(specialist.name));
    }
    map.insert(specialist.name.clone(), specialist);
    Ok(())
}

/// Keeps a recipe under its name, replacing one that is already there.
pub fn register_replace(specialist: Specialist) {
    registry()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(specialist.name.clone(), specialist);
}

/// Forgets a registered recipe (the built-in ones too; they stay available
/// as functions such as [`researcher`]).
pub fn unregister(name: &str) -> bool {
    registry()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .remove(name)
        .is_some()
}

/// The recipe registered under `name`.
pub fn get(name: &str) -> Result<Specialist, SpecialistError> {
    let map = registry().read().unwrap_or_else(|e| e.into_inner());
    map.get(name)
        .cloned()
        .ok_or_else(|| SpecialistError::Unknown {
            name: name.to_string(),
            registered: map.keys().cloned().collect(),
        })
}

/// Registered names, sorted.
pub fn available() -> Vec<String> {
    registry()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .keys()
        .cloned()
        .collect()
}

// ------------------------------------------------------------------ built-ins

fn builtin(name: &str, description: &str, instructions: &str) -> Specialist {
    Specialist::new(name, description, instructions).expect("built-in specialists are valid")
}

#[cfg(feature = "web")]
fn web_tools() -> Vec<Arc<dyn Tool>> {
    crate::web::default_tools()
}

#[cfg(not(feature = "web"))]
fn web_tools() -> Vec<Arc<dyn Tool>> {
    Vec::new()
}

/// Finds and gathers facts on a topic. With the `web` feature and a search
/// key in the environment it gets web search and page reading when it has no
/// tools of its own.
pub fn researcher() -> Specialist {
    builtin(
        "researcher",
        "Finds and gathers facts on a topic, with sources, using its tools. Use it before writing.",
        "You are a careful researcher. Use your tools to find facts; never invent facts or sources. \
         Reply with short bullet points, each with where it came from. If you cannot find something, say so.",
    )
    .default_tools(web_tools)
}

/// Condenses long text into a short, faithful summary.
pub fn summarizer() -> Specialist {
    builtin(
        "summarizer",
        "Condenses long text into a short, faithful summary.",
        "You summarize. Keep every important fact and number, drop the rest, add nothing that is not in the text. \
         Reply with the summary only.",
    )
}

/// Puts a message into one category and rates its urgency.
pub fn classifier() -> Specialist {
    builtin(
        "classifier",
        "Puts a message into one category and rates its urgency. Use it to triage.",
        "You classify the user's message. Choose exactly one category from the list you were given (if none was given, \
         use: billing, shipping, technical, other) and rate urgency as low, medium or high. Reply as JSON: \
         {\"category\": \"...\", \"urgency\": \"...\"}.",
    )
}

/// Pulls structured fields out of free text. Give it an output schema.
pub fn extractor() -> Specialist {
    builtin(
        "extractor",
        "Pulls structured fields out of free text. Give it an output schema.",
        "You extract fields from the user's text. Fill only what the text states; leave a field null when it is not there. \
         Never guess.",
    )
}

/// Breaks a goal into a short ordered list of concrete steps.
pub fn planner() -> Specialist {
    builtin(
        "planner",
        "Breaks a goal into a short ordered list of concrete steps.",
        "You plan. Break the goal into at most 7 concrete, ordered steps, each one a single action someone could do. \
         Reply with a numbered list only.",
    )
}

/// Writes clear prose from notes or a brief.
pub fn writer() -> Specialist {
    builtin(
        "writer",
        "Writes clear prose from notes or a brief. Use it after research.",
        "You are a writer. Turn the notes you are given into clear, well-organised prose for the stated audience. \
         Use only facts from the notes; if the notes are thin, say what is missing instead of filling it in.",
    )
}

/// Reviews a draft against criteria: `APPROVED`, or `REVISE:` and what to fix.
pub fn critic() -> Specialist {
    builtin(
        "critic",
        "Reviews a draft against criteria and either approves it or says what to fix.",
        "You are a strict reviewer. Check the draft against the criteria you were given (accuracy, completeness, clarity, \
         and anything the user specified). If it is good enough to use, reply with exactly: APPROVED. Otherwise reply with: \
         REVISE: followed by a short, specific list of what to change.",
    )
}

/// Checks the claims in a text with its tools.
pub fn fact_checker() -> Specialist {
    builtin(
        "fact_checker",
        "Checks claims in a text against its tools and reports which hold.",
        "You check facts. List each factual claim in the text, verify it with your tools, and label it SUPPORTED, \
         CONTRADICTED or UNVERIFIED with one line of evidence. Never mark a claim SUPPORTED without evidence.",
    )
}

/// Reviews code for bugs, security problems and unclear parts.
pub fn code_reviewer() -> Specialist {
    builtin(
        "code_reviewer",
        "Reviews code for bugs, security problems and unclear parts.",
        "You review code. Report real bugs first, then security problems, then readability, each with the line or \
         function and a concrete fix. If the code is fine, reply with exactly: APPROVED.",
    )
}

/// Translates text faithfully.
pub fn translator() -> Specialist {
    builtin(
        "translator",
        "Translates text faithfully into the language it is asked for.",
        "You translate. Keep the meaning, tone and formatting; do not add or remove information. Reply with the \
         translation only.",
    )
}

/// Answers customer questions politely and briefly.
pub fn support_agent() -> Specialist {
    builtin(
        "support_agent",
        "Answers customer questions politely and briefly, using its tools for facts.",
        "You are a friendly customer support agent. Be brief. Use your tools for facts about orders and accounts; never \
         guess. If an action needs a person's approval, say it is waiting for approval.",
    )
}
