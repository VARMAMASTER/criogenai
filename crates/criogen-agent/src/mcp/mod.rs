//! Tools from MCP servers (design 0022). Behind the `mcp` cargo feature
//! (Streamable HTTP, also in the browser); `mcp-stdio` adds servers started
//! as a local process.
//!
//! ```no_run
//! use criogen_agent::{Agent, mcp::McpServer};
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! # let model = criogen_providers::FakeModel::new();
//! let docs = McpServer::http("https://mcp.example.com/mcp")
//!     .header("Authorization", "Bearer …")
//!     .prefix("docs")
//!     .connect()
//!     .await?;
//! let agent = Agent::builder(model).tools(docs.tools()).build()?;
//! # Ok(()) }
//! ```
//!
//! The client speaks MCP 2026-07-28 (stateless, `server/discover`) and falls
//! back to the `initialize` handshake of 2025-11-25 and older. It only uses
//! tools: a server cannot sample the agent's model or read its files.
//!
//! What a server says is data: tool names are mapped to names models accept,
//! schemas are bounded, descriptions are cut, and its behaviour hints
//! (`readOnlyHint`, `idempotentHint`) decide nothing unless
//! [`McpServer::trust_annotations`] is set. Every message read is limited in
//! size and every request in time. Header and environment values never
//! appear in `Debug` output or an error.

mod http;
mod rpc;
#[cfg(feature = "mcp-stdio")]
mod stdio;

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use criogen_core::CancelToken;
use criogen_providers::{
    BoxFuture, ContentBlock, MediaSource, ToolAnnotations, ToolKind, ToolSpec,
};
use serde_json::{Map, Value, json};

use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};
use rpc::{Era, Failure};

/// The MCP revision this client speaks first.
pub const PROTOCOL_VERSION: &str = "2026-07-28";
/// The revision asked for in the legacy `initialize` handshake.
pub const LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
/// Revisions accepted from a legacy server's `initialize` answer.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// The most tools taken from one server.
pub const MAX_TOOLS: usize = 1_000;
/// The most `tools/list` pages read from one server.
pub const MAX_PAGES: usize = 100;
/// The deepest nesting allowed in a tool's input schema.
pub const MAX_SCHEMA_DEPTH: usize = 64;
/// The largest input schema accepted, in bytes of JSON.
pub const MAX_SCHEMA_BYTES: usize = 256 * 1024;
/// A tool's description is cut to this many characters.
pub const MAX_DESCRIPTION_CHARS: usize = 8_000;
/// Requests in flight to one server; more wait.
pub const MAX_IN_FLIGHT: usize = 64;

/// Why an MCP server could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum McpErrorKind {
    /// The server could not be started or reached, or refused the
    /// credentials.
    Connect,
    /// The server answered something this client cannot use (a malformed
    /// message, an unsupported protocol version, an error).
    Protocol,
    /// Two of its tools map to the same name, or a name maps to nothing.
    ToolName,
    /// A tool's input schema is not an object schema or is too large.
    ToolSchema,
    /// The server did not answer in time.
    Timeout,
    /// The server stopped (the process exited, or the client was closed).
    Closed,
}

impl McpErrorKind {
    /// The stable code (`schemas/error-codes.json`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Connect => "mcp_connect",
            Self::Protocol => "mcp_protocol",
            Self::ToolName => "mcp_tool_name",
            Self::ToolSchema => "mcp_tool_schema",
            Self::Timeout => "mcp_timeout",
            Self::Closed => "mcp_closed",
        }
    }
}

/// An MCP server could not be connected to or used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct McpError {
    /// What kind of failure.
    pub kind: McpErrorKind,
    /// What happened, naming the server. Never holds a header or
    /// environment value.
    pub message: String,
}

impl McpError {
    fn new(kind: McpErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// The stable code, e.g. `mcp_connect`.
    pub fn code(&self) -> &'static str {
        self.kind.code()
    }
}

#[derive(Clone)]
enum Target {
    #[cfg_attr(not(feature = "mcp-stdio"), allow(dead_code))]
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<std::path::PathBuf>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

/// How to reach an MCP server. Build it with [`stdio`](Self::stdio) or
/// [`http`](Self::http), then [`connect`](Self::connect).
#[derive(Clone)]
pub struct McpServer {
    target: Target,
    name: Option<String>,
    prefix: Option<String>,
    timeout: Duration,
    connect_timeout: Duration,
    max_message_bytes: usize,
    trust_annotations: bool,
    only: Option<Vec<String>>,
    except: Vec<String>,
}

impl std::fmt::Debug for McpServer {
    /// Header and environment values are not shown (they hold tokens).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("McpServer");
        match &self.target {
            Target::Stdio {
                command, args, env, ..
            } => s
                .field("command", command)
                .field("args", &args.len())
                .field("env", &env.iter().map(|(k, _)| k).collect::<Vec<_>>()),
            Target::Http { url, headers } => s.field("url", url).field(
                "headers",
                &headers.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            ),
        };
        s.field("name", &self.name)
            .field("prefix", &self.prefix)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl McpServer {
    fn with(target: Target) -> Self {
        Self {
            target,
            name: None,
            prefix: None,
            timeout: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(30),
            max_message_bytes: 4 * 1024 * 1024,
            trust_annotations: false,
            only: None,
            except: Vec::new(),
        }
    }

    /// A server this app starts: `command` with `args`, speaking MCP on its
    /// stdin and stdout (feature `mcp-stdio`; not in the browser). It
    /// inherits this process's environment, plus [`env`](Self::env).
    #[cfg(feature = "mcp-stdio")]
    pub fn stdio<I, S>(command: impl Into<String>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::with(Target::Stdio {
            command: command.into(),
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
            cwd: None,
        })
    }

    /// A server at `url` (Streamable HTTP). Use `https`; plain `http` only
    /// to this computer when headers are set.
    pub fn http(url: impl Into<String>) -> Self {
        Self::with(Target::Http {
            url: url.into(),
            headers: Vec::new(),
        })
    }

    /// The server's name in errors (default: the prefix, else the command
    /// or the host).
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets an environment variable for a stdio server (ignored for HTTP).
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        if let Target::Stdio { env, .. } = &mut self.target {
            env.push((key.into(), value.into()));
        }
        self
    }

    /// The working directory of a stdio server (ignored for HTTP).
    pub fn cwd(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        if let Target::Stdio { cwd, .. } = &mut self.target {
            *cwd = Some(dir.into());
        }
        self
    }

    /// Adds a header to every request to an HTTP server (ignored for stdio),
    /// e.g. `Authorization`.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        if let Target::Http { headers, .. } = &mut self.target {
            headers.push((name.into(), value.into()));
        }
        self
    }

    /// The agent sees `<prefix>_<tool>`: keeps two servers' tools apart.
    pub fn prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// The longest one request (a tool call) may take. Default 60 s.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The longest [`connect`](Self::connect) may take. Default 30 s.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// The largest message read from the server. Default 4 MiB.
    pub fn max_message_bytes(mut self, bytes: usize) -> Self {
        self.max_message_bytes = bytes.max(1024);
        self
    }

    /// Believe the server's `readOnlyHint` and `idempotentHint` (they decide
    /// whether a failed call is retried). Off by default: a server's hints
    /// are its own claims.
    pub fn trust_annotations(mut self, trust: bool) -> Self {
        self.trust_annotations = trust;
        self
    }

    /// Takes only these tools (the server's names).
    pub fn only<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.only = Some(names.into_iter().map(Into::into).collect());
        self
    }

    /// Leaves these tools out (the server's names).
    pub fn except<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.except.extend(names.into_iter().map(Into::into));
        self
    }

    /// A server from its JSON description, for the SDK bridges:
    /// `{"transport": "stdio" | "http", "command", "args", "env": {…},
    /// "cwd", "url", "headers": {…}, "name", "prefix", "timeout_secs",
    /// "connect_timeout_secs", "max_message_bytes", "trust_annotations",
    /// "only": […], "exclude": […]}`.
    pub fn from_spec(spec: &Value) -> Result<Self, McpError> {
        let text = |key: &str| spec.get(key).and_then(Value::as_str);
        let strings = |key: &str| string_list(spec.get(key));
        let pairs = |key: &str| -> Vec<(String, String)> {
            spec.get(key)
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut server = match text("transport") {
            Some("http") => {
                let mut server = Self::http(text("url").unwrap_or_default());
                for (k, v) in pairs("headers") {
                    server = server.header(k, v);
                }
                server
            }
            #[cfg(feature = "mcp-stdio")]
            Some("stdio") => {
                let mut server = Self::stdio(text("command").unwrap_or_default(), strings("args"));
                for (k, v) in pairs("env") {
                    server = server.env(k, v);
                }
                if let Some(cwd) = text("cwd") {
                    server = server.cwd(cwd);
                }
                server
            }
            #[cfg(not(feature = "mcp-stdio"))]
            Some("stdio") => {
                return Err(McpError::new(
                    McpErrorKind::Connect,
                    "MCP servers started as a process (stdio) need a server; here only `http` \
                     servers can be used",
                ));
            }
            _ => {
                return Err(McpError::new(
                    McpErrorKind::Connect,
                    "an MCP server needs `transport`: \"stdio\" or \"http\"",
                ));
            }
        };
        if let Some(name) = text("name") {
            server = server.name(name);
        }
        if let Some(prefix) = text("prefix") {
            server = server.prefix(prefix);
        }
        let secs = |key: &str| {
            spec.get(key)
                .and_then(Value::as_f64)
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(|s| Duration::from_secs_f64(s.min(86_400.0)))
        };
        if let Some(t) = secs("timeout_secs") {
            server = server.timeout(t);
        }
        if let Some(t) = secs("connect_timeout_secs") {
            server = server.connect_timeout(t);
        }
        if let Some(bytes) = spec.get("max_message_bytes").and_then(Value::as_u64) {
            server = server.max_message_bytes(usize::try_from(bytes).unwrap_or(usize::MAX));
        }
        if spec.get("trust_annotations").and_then(Value::as_bool) == Some(true) {
            server = server.trust_annotations(true);
        }
        if spec.get("only").is_some_and(Value::is_array) {
            server = server.only(strings("only"));
        }
        server = server.except(strings("exclude"));
        Ok(server)
    }

    fn label(&self) -> String {
        if let Some(name) = self.name.as_ref().or(self.prefix.as_ref()) {
            return name.clone();
        }
        match &self.target {
            Target::Stdio { command, .. } => std::path::Path::new(command)
                .file_stem()
                .map_or_else(|| command.clone(), |s| s.to_string_lossy().into_owned()),
            Target::Http { url, .. } => rpc::host_of(url).unwrap_or("server").to_string(),
        }
    }

    /// Starts or reaches the server, agrees on a protocol version and lists
    /// its tools.
    pub async fn connect(self) -> Result<McpClient, McpError> {
        let label = self.label();
        let conn = match &self.target {
            #[cfg(feature = "mcp-stdio")]
            Target::Stdio {
                command,
                args,
                env,
                cwd,
            } => Conn::Stdio(stdio::Stdio::new(
                &label,
                command.clone(),
                args.clone(),
                env.clone(),
                cwd.clone(),
                self.max_message_bytes,
            )),
            #[cfg(not(feature = "mcp-stdio"))]
            Target::Stdio { .. } => unreachable!("stdio servers need the `mcp-stdio` feature"),
            Target::Http { url, headers } => Conn::Http(http::Http::new(
                &label,
                url,
                headers.clone(),
                self.max_message_bytes,
                self.timeout.max(self.connect_timeout),
            )?),
        };
        let connect_timeout = self.connect_timeout;
        let inner = Arc::new(Inner {
            label,
            config: self,
            conn,
            era: RwLock::new(Era::Modern),
            info: RwLock::new(ServerInfo::default()),
            tools: RwLock::new(Vec::new()),
            permits: tokio::sync::Semaphore::new(MAX_IN_FLIGHT),
            restart: tokio::sync::Mutex::new(()),
            next_id: AtomicI64::new(1),
        });
        let setup = async {
            inner.start().await?;
            inner.handshake().await?;
            inner.list_tools().await
        };
        match criogen_rt::timeout(connect_timeout, setup).await {
            Ok(Ok(())) => Ok(McpClient { inner }),
            Ok(Err(error)) => {
                inner.conn.close().await;
                Err(error)
            }
            Err(_) => {
                inner.conn.close().await;
                Err(McpError::new(
                    McpErrorKind::Timeout,
                    format!(
                        "MCP server \"{}\" was not ready within {} ms",
                        inner.label,
                        connect_timeout.as_millis()
                    ),
                ))
            }
        }
    }
}

/// What a server said about itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServerInfo {
    /// Its name (`serverInfo.name`).
    pub name: Option<String>,
    /// Its version.
    pub version: Option<String>,
    /// The MCP revision in use.
    pub protocol_version: String,
    /// Its instructions for models (data from the server, not added to any
    /// prompt by Criogen).
    pub instructions: Option<String>,
}

/// A connected MCP server. Cheap to clone; the server stops (a stdio
/// process exits) when the last clone and the last of its tools are
/// dropped, or at [`close`](Self::close).
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field("server", &self.inner.label)
            .field("tools", &self.tool_names())
            .finish_non_exhaustive()
    }
}

impl McpClient {
    /// The server's tools, for [`AgentBuilder::tools`](crate::AgentBuilder::tools).
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        read(&self.inner.tools)
            .iter()
            .map(|def| {
                Arc::new(McpTool {
                    def: def.clone(),
                    inner: self.inner.clone(),
                }) as Arc<dyn Tool>
            })
            .collect()
    }

    /// The names the agent sees, in the server's order.
    pub fn tool_names(&self) -> Vec<String> {
        read(&self.inner.tools)
            .iter()
            .map(|def| def.spec.name.clone())
            .collect()
    }

    /// The server and its tools as JSON, for the SDK bridges: `{"info":
    /// {"name", "version", "protocol_version", "instructions"}, "tools":
    /// [{"name", "description", "schema", "read_only", "destructive",
    /// "idempotent"}]}`.
    pub fn describe(&self) -> Value {
        let info = self.info();
        let tools: Vec<Value> = read(&self.inner.tools)
            .iter()
            .map(|def| {
                let spec = &def.spec;
                json!({
                    "name": spec.name,
                    "description": spec.description,
                    "schema": spec.input_schema,
                    "read_only": spec.annotations.read_only,
                    "destructive": spec.annotations.destructive,
                    "idempotent": spec.annotations.idempotent,
                })
            })
            .collect();
        json!({
            "info": {
                "name": info.name,
                "version": info.version,
                "protocol_version": info.protocol_version,
                "instructions": info.instructions,
            },
            "tools": tools,
        })
    }

    /// What the server said about itself.
    pub fn info(&self) -> ServerInfo {
        read(&self.inner.info).clone()
    }

    /// Lists the tools again (after the server changed them). Tools already
    /// given to agents keep their definitions.
    pub async fn refresh(&self) -> Result<(), McpError> {
        self.inner.list_tools().await
    }

    /// Calls a tool by the name the agent sees, outside any agent.
    pub async fn call(
        &self,
        name: &str,
        args: Map<String, Value>,
    ) -> Result<ToolOutput, ToolError> {
        let def = read(&self.inner.tools)
            .iter()
            .find(|def| def.spec.name == name)
            .cloned()
            .ok_or_else(|| ToolError::new(format!("no tool named `{name}`")))?;
        self.inner.call_tool(&def, args, None).await
    }

    /// Stops the server (a stdio process: stdin closed, then killed after
    /// 2 s). Later calls fail with `mcp_closed`.
    pub async fn close(&self) {
        self.inner.conn.close().await;
    }
}

fn read<T: Clone>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write<T>(lock: &RwLock<T>, value: T) {
    *lock
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
}

enum Conn {
    #[cfg(feature = "mcp-stdio")]
    Stdio(stdio::Stdio),
    Http(http::Http),
}

impl Conn {
    async fn close(&self) {
        match self {
            #[cfg(feature = "mcp-stdio")]
            Self::Stdio(s) => s.close().await,
            Self::Http(h) => h.close(),
        }
    }
}

struct Inner {
    label: String,
    config: McpServer,
    conn: Conn,
    era: RwLock<Era>,
    info: RwLock<ServerInfo>,
    tools: RwLock<Vec<Arc<ToolDef>>>,
    permits: tokio::sync::Semaphore,
    restart: tokio::sync::Mutex<()>,
    next_id: AtomicI64,
}

/// A tool as the agent sees it, and how to call it on the server.
#[derive(Debug)]
struct ToolDef {
    spec: ToolSpec,
    remote: String,
    /// `(argument, header name)` for `x-mcp-header` properties.
    header_params: Vec<(String, String)>,
}

impl Inner {
    fn era(&self) -> Era {
        read(&self.era).clone()
    }

    async fn start(&self) -> Result<(), McpError> {
        match &self.conn {
            #[cfg(feature = "mcp-stdio")]
            Conn::Stdio(s) => s.start().await,
            Conn::Http(_) => Ok(()),
        }
    }

    /// One request, without the in-flight limit or restarts.
    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        era: &Era,
        method: &str,
        params: Map<String, Value>,
        name: Option<&str>,
        headers: &[(String, String)],
        cancel: Option<&CancelToken>,
        timeout: Duration,
    ) -> Result<Value, Failure> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = rpc::request(era, id, method, params);
        match &self.conn {
            #[cfg(feature = "mcp-stdio")]
            Conn::Stdio(s) => s.request(id, &message, cancel, timeout).await,
            Conn::Http(h) => {
                h.request(era, id, method, &message, name, headers, cancel, timeout)
                    .await
            }
        }
    }

    async fn notify(&self, era: &Era, method: &str) {
        let message = json!({"jsonrpc": "2.0", "method": method});
        match &self.conn {
            #[cfg(feature = "mcp-stdio")]
            Conn::Stdio(s) => s.notify(&message),
            Conn::Http(h) => h.notify(era, method, &message, self.config.timeout).await,
        }
    }

    /// Finds out which era the server is from and does its handshake.
    async fn handshake(&self) -> Result<(), McpError> {
        let probe_timeout = self.config.connect_timeout.min(Duration::from_secs(10));
        let probe = self
            .send(
                &Era::Modern,
                "server/discover",
                Map::new(),
                None,
                &[],
                None,
                probe_timeout,
            )
            .await;
        let legacy = match probe {
            Ok(result) => {
                let supported = string_list(result.get("supportedVersions"));
                if supported.iter().any(|v| v == PROTOCOL_VERSION) {
                    let server = result
                        .get("_meta")
                        .and_then(|m| m.get(rpc::META_SERVER_INFO))
                        .or_else(|| result.get("serverInfo"));
                    write(
                        &self.info,
                        ServerInfo {
                            name: text_at(server, "name"),
                            version: text_at(server, "version"),
                            protocol_version: PROTOCOL_VERSION.to_string(),
                            instructions: text_at(Some(&result), "instructions"),
                        },
                    );
                    write(&self.era, Era::Modern);
                    return Ok(());
                }
                if !supported
                    .iter()
                    .any(|v| LEGACY_VERSIONS.contains(&v.as_str()))
                {
                    return Err(self.unsupported(&supported));
                }
                true
            }
            Err(Failure::Rpc { code, data, .. }) if code == rpc::UNSUPPORTED_VERSION => {
                let supported = string_list(data.as_ref().and_then(|d| d.get("supported")));
                if !supported.is_empty()
                    && !supported
                        .iter()
                        .any(|v| LEGACY_VERSIONS.contains(&v.as_str()))
                {
                    return Err(self.unsupported(&supported));
                }
                true
            }
            // Any other answer: a server from before `server/discover`.
            Err(
                Failure::Rpc { .. }
                | Failure::Status(_)
                | Failure::Timeout
                | Failure::Closed(_)
                | Failure::SessionExpired
                | Failure::Protocol(_),
            ) => true,
            Err(failure) => return Err(self.connect_error(failure)),
        };
        if legacy {
            self.initialize().await?;
        }
        Ok(())
    }

    fn unsupported(&self, supported: &[String]) -> McpError {
        McpError::new(
            McpErrorKind::Protocol,
            format!(
                "MCP server \"{}\" speaks protocol versions {supported:?}; Criogen speaks \
                 {PROTOCOL_VERSION} and {}",
                self.label,
                LEGACY_VERSIONS.join(", ")
            ),
        )
    }

    /// The legacy handshake: `initialize`, then `notifications/initialized`.
    async fn initialize(&self) -> Result<(), McpError> {
        #[cfg(feature = "mcp-stdio")]
        if let Conn::Stdio(s) = &self.conn
            && !s.alive()
        {
            // A legacy server that exited on `server/discover`.
            s.start().await?;
        }
        let mut params = Map::new();
        params.insert("protocolVersion".into(), json!(LEGACY_PROTOCOL_VERSION));
        params.insert("capabilities".into(), json!({}));
        params.insert("clientInfo".into(), rpc::client_info());
        let era = Era::Legacy(LEGACY_PROTOCOL_VERSION.to_string());
        let result = self
            .send(
                &era,
                "initialize",
                params,
                None,
                &[],
                None,
                self.config.connect_timeout,
            )
            .await
            .map_err(|f| self.connect_error(f))?;
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !LEGACY_VERSIONS.contains(&version.as_str()) {
            return Err(self.unsupported(&[version]));
        }
        let server = result.get("serverInfo");
        write(
            &self.info,
            ServerInfo {
                name: text_at(server, "name"),
                version: text_at(server, "version"),
                protocol_version: version.clone(),
                instructions: text_at(Some(&result), "instructions"),
            },
        );
        let era = Era::Legacy(version);
        write(&self.era, era.clone());
        self.notify(&era, "notifications/initialized").await;
        Ok(())
    }

    fn connect_error(&self, failure: Failure) -> McpError {
        let kind = match &failure {
            Failure::Timeout => McpErrorKind::Timeout,
            Failure::Closed(_) => McpErrorKind::Closed,
            Failure::Connect(_) | Failure::Auth(_) => McpErrorKind::Connect,
            _ => McpErrorKind::Protocol,
        };
        McpError::new(
            kind,
            format!("MCP server \"{}\": {}", self.label, failure.describe(true)),
        )
    }

    /// A request with the in-flight limit, a restart of a stopped stdio
    /// server and a new legacy HTTP session when the old one expired.
    async fn request(
        &self,
        method: &str,
        params: Map<String, Value>,
        name: Option<&str>,
        headers: &[(String, String)],
        cancel: Option<&CancelToken>,
    ) -> Result<Value, Failure> {
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| Failure::Closed("the client was closed".into()))?;
        self.ensure_running().await?;
        let timeout = self.config.timeout;
        let era = self.era();
        match self
            .send(&era, method, params.clone(), name, headers, cancel, timeout)
            .await
        {
            Err(Failure::SessionExpired) => {
                let _guard = self.restart.lock().await;
                self.initialize()
                    .await
                    .map_err(|e| Failure::Connect(e.message))?;
                let era = self.era();
                self.send(&era, method, params, name, headers, cancel, timeout)
                    .await
            }
            other => other,
        }
    }

    async fn ensure_running(&self) -> Result<(), Failure> {
        #[cfg(feature = "mcp-stdio")]
        if let Conn::Stdio(s) = &self.conn
            && !s.alive()
        {
            let _guard = self.restart.lock().await;
            if !s.alive() {
                if s.closed() {
                    return Err(Failure::Closed("the client was closed".into()));
                }
                s.start().await.map_err(|e| Failure::Closed(e.message))?;
                if let Era::Legacy(_) = self.era() {
                    self.initialize()
                        .await
                        .map_err(|e| Failure::Closed(e.message))?;
                }
            }
        }
        Ok(())
    }

    async fn list_tools(&self) -> Result<(), McpError> {
        let mut raw = Vec::new();
        let mut cursor: Option<String> = None;
        for page in 0.. {
            if page == MAX_PAGES {
                return Err(McpError::new(
                    McpErrorKind::Protocol,
                    format!(
                        "MCP server \"{}\" listed tools over more than {MAX_PAGES} pages",
                        self.label
                    ),
                ));
            }
            let mut params = Map::new();
            if let Some(cursor) = &cursor {
                params.insert("cursor".into(), json!(cursor));
            }
            let result = match self.request("tools/list", params, None, &[], None).await {
                Ok(result) => result,
                // A server without tools.
                Err(Failure::Rpc { code, .. }) if code == rpc::METHOD_NOT_FOUND && page == 0 => {
                    break;
                }
                Err(failure) => return Err(self.connect_error(failure)),
            };
            let Some(tools) = result.get("tools").and_then(Value::as_array) else {
                return Err(McpError::new(
                    McpErrorKind::Protocol,
                    format!(
                        "MCP server \"{}\" answered tools/list without a `tools` list",
                        self.label
                    ),
                ));
            };
            raw.extend(tools.iter().cloned());
            if raw.len() > MAX_TOOLS {
                return Err(McpError::new(
                    McpErrorKind::Protocol,
                    format!(
                        "MCP server \"{}\" has more than {MAX_TOOLS} tools; use `only` to pick some",
                        self.label
                    ),
                ));
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !next.is_empty() => cursor = Some(next.to_string()),
                _ => break,
            }
        }
        let defs = map_tools(&self.label, &self.config, raw)?;
        write(&self.tools, defs.into_iter().map(Arc::new).collect());
        Ok(())
    }

    async fn call_tool(
        &self,
        def: &ToolDef,
        args: Map<String, Value>,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolError> {
        let headers: Vec<(String, String)> = def
            .header_params
            .iter()
            .filter_map(|(arg, header)| {
                let value = match args.get(arg)? {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => return None,
                };
                Some((format!("Mcp-Param-{header}"), rpc::header_value(&value)))
            })
            .collect();
        let mut params = Map::new();
        params.insert("name".into(), json!(def.remote));
        params.insert("arguments".into(), Value::Object(args));
        match self
            .request("tools/call", params, Some(&def.remote), &headers, cancel)
            .await
        {
            Ok(result) => to_output(&self.label, &result),
            Err(failure) => {
                let retry = matches!(failure, Failure::Timeout | Failure::Closed(_));
                let error = ToolError::new(format!(
                    "MCP server \"{}\": {}",
                    self.label,
                    failure.describe(false)
                ));
                Err(if retry { error.retryable() } else { error })
            }
        }
    }
}

struct McpTool {
    def: Arc<ToolDef>,
    inner: Arc<Inner>,
}

impl std::fmt::Debug for McpTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTool")
            .field("name", &self.def.spec.name)
            .field("server", &self.inner.label)
            .finish()
    }
}

impl Tool for McpTool {
    fn spec(&self) -> &ToolSpec {
        &self.def.spec
    }

    fn call(
        &self,
        args: Map<String, Value>,
        ctx: ToolContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            self.inner
                .call_tool(&self.def, args, Some(ctx.cancel_token()))
                .await
        })
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn text_at(value: Option<&Value>, key: &str) -> Option<String> {
    value?.get(key)?.as_str().map(|s| cut(s, 2_000))
}

/// The first `max` characters of `s`.
fn cut(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &s[..at]),
        None => s.to_string(),
    }
}

/// The name a model accepts for `name`: `[A-Za-z0-9_-]`, at most 64.
fn local_name(prefix: Option<&str>, name: &str) -> String {
    let joined = match prefix {
        Some(p) if !p.is_empty() => format!("{p}_{name}"),
        _ => name.to_string(),
    };
    joined
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn schema_depth(value: &Value, depth: usize) -> usize {
    if depth > MAX_SCHEMA_DEPTH {
        return depth;
    }
    match value {
        Value::Object(map) => map
            .values()
            .map(|v| schema_depth(v, depth + 1))
            .max()
            .unwrap_or(depth),
        Value::Array(items) => items
            .iter()
            .map(|v| schema_depth(v, depth + 1))
            .max()
            .unwrap_or(depth),
        _ => depth,
    }
}

fn map_tools(label: &str, config: &McpServer, raw: Vec<Value>) -> Result<Vec<ToolDef>, McpError> {
    let mut defs = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for tool in raw {
        let Some(remote) = tool.get("name").and_then(Value::as_str) else {
            return Err(McpError::new(
                McpErrorKind::Protocol,
                format!("MCP server \"{label}\" listed a tool without a name"),
            ));
        };
        if let Some(only) = &config.only
            && !only.iter().any(|n| n == remote)
        {
            continue;
        }
        if config.except.iter().any(|n| n == remote) {
            continue;
        }
        let shown = cut(remote, 128);
        let name = local_name(config.prefix.as_deref(), remote);
        if name.is_empty() || name.chars().all(|c| c == '_') {
            return Err(McpError::new(
                McpErrorKind::ToolName,
                format!(
                    "MCP server \"{label}\": tool `{shown}` has no usable name; set a prefix or \
                     leave it out with `except`"
                ),
            ));
        }
        if !seen.insert(name.clone()) {
            return Err(McpError::new(
                McpErrorKind::ToolName,
                format!(
                    "MCP server \"{label}\": two tools map to the name `{name}`; set a prefix or \
                     leave one out with `except`"
                ),
            ));
        }
        let schema = tool.get("inputSchema").cloned().unwrap_or(Value::Null);
        let size = serde_json::to_vec(&schema).map_or(usize::MAX, |b| b.len());
        let problem = if size > MAX_SCHEMA_BYTES {
            Some(format!("is larger than {MAX_SCHEMA_BYTES} bytes"))
        } else if schema_depth(&schema, 0) > MAX_SCHEMA_DEPTH {
            Some(format!("is nested deeper than {MAX_SCHEMA_DEPTH} levels"))
        } else if schema.get("type").and_then(Value::as_str) != Some("object") {
            Some("is not an object schema (`\"type\": \"object\"`)".to_string())
        } else {
            None
        };
        if let Some(problem) = problem {
            return Err(McpError::new(
                McpErrorKind::ToolSchema,
                format!(
                    "MCP server \"{label}\": the input schema of tool `{shown}` {problem}; leave \
                     it out with `except`"
                ),
            ));
        }
        let header_params = schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|props| {
                props
                    .iter()
                    .filter_map(|(arg, prop)| {
                        let header = prop.get("x-mcp-header")?.as_str()?;
                        let ok = !header.is_empty()
                            && header.len() <= 64
                            && header
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
                        ok.then(|| (arg.clone(), header.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .or_else(|| tool.get("title").and_then(Value::as_str))
            .map(|d| cut(d, MAX_DESCRIPTION_CHARS))
            .unwrap_or_default();
        let hints = tool.get("annotations");
        let hint =
            |key: &str| hints.and_then(|h| h.get(key)).and_then(Value::as_bool) == Some(true);
        let mut annotations = ToolAnnotations::default();
        annotations.destructive = hint("destructiveHint");
        if config.trust_annotations {
            annotations.read_only = hint("readOnlyHint");
            annotations.idempotent = hint("idempotentHint") || annotations.read_only;
        }
        let mut spec = ToolSpec::new(name, description, schema).with_annotations(annotations);
        spec.kind = ToolKind::Mcp;
        defs.push(ToolDef {
            spec,
            remote: remote.to_string(),
            header_params,
        });
    }
    Ok(defs)
}

fn block(value: &Value) -> Option<ContentBlock> {
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    match text("type")? {
        "text" => Some(ContentBlock::text(text("text").unwrap_or_default())),
        "image" => Some(ContentBlock::image(
            MediaSource::Base64 {
                data: text("data")?.to_string(),
            },
            text("mimeType").map(str::to_string),
        )),
        "audio" => Some(ContentBlock::audio(
            MediaSource::Base64 {
                data: text("data")?.to_string(),
            },
            text("mimeType").map(str::to_string),
        )),
        "resource_link" => {
            let uri = text("uri").unwrap_or_default();
            Some(ContentBlock::text(match text("name") {
                Some(name) => format!("resource: {uri} ({name})"),
                None => format!("resource: {uri}"),
            }))
        }
        "resource" => {
            let resource = value.get("resource")?;
            let uri = resource
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(ContentBlock::text(
                match resource.get("text").and_then(Value::as_str) {
                    Some(text) => text.to_string(),
                    None => {
                        let mime = resource
                            .get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("binary data");
                        format!("resource: {uri} ({mime}, not shown)")
                    }
                },
            ))
        }
        _ => None,
    }
}

fn to_output(label: &str, result: &Value) -> Result<ToolOutput, ToolError> {
    if let Some(kind) = result.get("resultType").and_then(Value::as_str)
        && kind != "complete"
    {
        return Err(ToolError::new(format!(
            "MCP server \"{label}\" asked for more input (`{}`), which Criogen does not support yet",
            cut(kind, 40)
        )));
    }
    let blocks: Vec<ContentBlock> = result
        .get("content")
        .and_then(Value::as_array)
        .map(|content| content.iter().filter_map(block).collect())
        .unwrap_or_default();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        let text: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        return Err(ToolError::new(if text.is_empty() {
            "the tool failed".to_string()
        } else {
            text.join("\n")
        }));
    }
    if blocks.is_empty() {
        return Ok(match result.get("structuredContent") {
            Some(value) if !value.is_null() => ToolOutput::Json(value.clone()),
            _ => ToolOutput::Text(String::new()),
        });
    }
    if let [ContentBlock::Text { text, .. }] = blocks.as_slice() {
        return Ok(ToolOutput::Text(text.clone()));
    }
    Ok(ToolOutput::Blocks(blocks))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_mapped_to_what_models_accept() {
        assert_eq!(local_name(None, "files.read"), "files_read");
        assert_eq!(local_name(Some("gh"), "issues/list"), "gh_issues_list");
        assert_eq!(local_name(None, &"a".repeat(200)).len(), 64);
        assert_eq!(local_name(None, "ünïcode"), "_n_code");
    }

    #[test]
    fn results_become_tool_outputs() {
        let out = to_output("s", &json!({"content": [{"type": "text", "text": "hi"}]}));
        assert_eq!(out.unwrap(), ToolOutput::Text("hi".into()));
        let out = to_output("s", &json!({"content": [], "structuredContent": {"a": 1}}));
        assert_eq!(out.unwrap(), ToolOutput::Json(json!({"a": 1})));
        let err = to_output(
            "s",
            &json!({"content": [{"type": "text", "text": "bad"}], "isError": true}),
        );
        assert_eq!(err.unwrap_err().message, "bad");
        let err = to_output("s", &json!({"resultType": "input_required"}));
        assert!(err.unwrap_err().message.contains("input_required"));
    }

    #[test]
    fn deep_schemas_are_measured() {
        let mut schema = json!({"type": "object"});
        for _ in 0..80 {
            schema = json!({"type": "object", "properties": {"x": schema}});
        }
        assert!(schema_depth(&schema, 0) > MAX_SCHEMA_DEPTH);
    }
}
