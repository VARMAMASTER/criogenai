//! stdio (design 0022): the server is a child process; one JSON message per
//! line on its stdin and stdout. Its stderr is its log.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogenai_core::CancelToken;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use super::rpc::{self, Failure, Stop};
use super::{McpError, McpErrorKind};

/// stderr lines kept, and their length.
const LOG_LINES: usize = 20;
const LOG_LINE_CHARS: usize = 500;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

enum Out {
    Line(Vec<u8>),
    Close,
}

struct Process {
    out: mpsc::UnboundedSender<Out>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    /// Why it stopped (set by the reader).
    ended: Arc<Mutex<Option<String>>>,
    child: Mutex<Option<Child>>,
}

pub(super) struct Stdio {
    label: String,
    command: String,
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<std::path::PathBuf>,
    max: usize,
    process: Mutex<Option<Arc<Process>>>,
    log: Arc<Mutex<VecDeque<String>>>,
    closed: AtomicBool,
}

impl Stdio {
    pub(super) fn new(
        label: &str,
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<std::path::PathBuf>,
        max: usize,
    ) -> Self {
        Self {
            label: label.to_string(),
            command,
            args,
            env,
            cwd,
            max,
            process: Mutex::new(None),
            log: Arc::new(Mutex::new(VecDeque::new())),
            closed: AtomicBool::new(false),
        }
    }

    pub(super) fn alive(&self) -> bool {
        lock(&self.process)
            .as_ref()
            .is_some_and(|p| p.alive.load(Ordering::Acquire))
    }

    pub(super) fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .args(&self.args)
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
    }

    /// The last lines of the server's log, without the env values given to
    /// it.
    fn log_tail(&self) -> String {
        let mut text = lock(&self.log)
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        for (_, value) in &self.env {
            if value.len() >= 4 {
                text = text.replace(value.as_str(), "[redacted]");
            }
        }
        text
    }

    pub(super) async fn start(&self) -> Result<(), McpError> {
        if self.closed() {
            return Err(McpError::new(
                McpErrorKind::Closed,
                format!("MCP server \"{}\" was closed", self.label),
            ));
        }
        let mut spawned = self.command(&self.command).spawn();
        // `npx`, `uvx` and friends are `.cmd` scripts on Windows.
        if cfg!(windows)
            && matches!(&spawned, Err(e) if e.kind() == std::io::ErrorKind::NotFound)
            && std::path::Path::new(&self.command).extension().is_none()
        {
            spawned = self.command(&format!("{}.cmd", self.command)).spawn();
        }
        let mut child = spawned.map_err(|e| {
            McpError::new(
                McpErrorKind::Connect,
                format!(
                    "MCP server \"{}\": could not start `{}`: {e}",
                    self.label, self.command
                ),
            )
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(McpError::new(
                McpErrorKind::Connect,
                format!("MCP server \"{}\": no pipes to the process", self.label),
            ));
        };

        let (out, mut lines) = mpsc::unbounded_channel::<Out>();
        criogenai_rt::spawn(async move {
            let mut stdin = stdin;
            while let Some(Out::Line(line)) = lines.recv().await {
                if stdin.write_all(&line).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
            // Dropping stdin tells the server to stop.
        });

        lock(&self.log).clear();
        let log = criogenai_rt::spawn(keep_log(stderr, self.log.clone()));

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let ended = Arc::new(Mutex::new(None));
        criogenai_rt::spawn(read_messages(
            stdout,
            self.max,
            pending.clone(),
            alive.clone(),
            ended.clone(),
            out.clone(),
            log,
        ));
        *lock(&self.process) = Some(Arc::new(Process {
            out,
            pending,
            alive,
            ended,
            child: Mutex::new(Some(child)),
        }));
        Ok(())
    }

    fn current(&self) -> Result<Arc<Process>, Failure> {
        lock(&self.process)
            .clone()
            .ok_or_else(|| Failure::Closed("the server is not running".into()))
    }

    fn stopped(&self, process: &Process) -> Failure {
        let why = lock(&process.ended)
            .clone()
            .unwrap_or_else(|| "the server process exited".into());
        let log = self.log_tail();
        Failure::Closed(if log.is_empty() {
            why
        } else {
            format!("{why}; its last log lines:\n{log}")
        })
    }

    pub(super) async fn request(
        &self,
        id: i64,
        message: &Value,
        cancel: Option<&CancelToken>,
        timeout: Duration,
    ) -> Result<Value, Failure> {
        let process = self.current()?;
        if !process.alive.load(Ordering::Acquire) {
            return Err(self.stopped(&process));
        }
        let (tx, rx) = oneshot::channel();
        lock(&process.pending).insert(id, tx);
        let mut line = serde_json::to_vec(message).unwrap_or_default();
        line.push(b'\n');
        if process.out.send(Out::Line(line)).is_err() {
            lock(&process.pending).remove(&id);
            return Err(self.stopped(&process));
        }
        match rpc::bounded(rx, cancel, timeout).await {
            Ok(Ok(answer)) => rpc::response(&answer),
            Ok(Err(_)) => Err(self.stopped(&process)),
            Err(stop) => {
                lock(&process.pending).remove(&id);
                let reason = match stop {
                    Stop::Timeout => "timed out",
                    Stop::Cancelled => "cancelled",
                };
                let note = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": id, "reason": reason},
                });
                let mut line = serde_json::to_vec(&note).unwrap_or_default();
                line.push(b'\n');
                let _ = process.out.send(Out::Line(line));
                Err(match stop {
                    Stop::Timeout => Failure::Timeout,
                    Stop::Cancelled => Failure::Cancelled,
                })
            }
        }
    }

    pub(super) fn notify(&self, message: &Value) {
        if let Ok(process) = self.current() {
            let mut line = serde_json::to_vec(message).unwrap_or_default();
            line.push(b'\n');
            let _ = process.out.send(Out::Line(line));
        }
    }

    pub(super) async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let Some(process) = lock(&self.process).take() else {
            return;
        };
        let _ = process.out.send(Out::Close);
        let child = lock(&process.child).take();
        if let Some(mut child) = child
            && criogenai_rt::timeout(Duration::from_secs(2), child.wait())
                .await
                .is_err()
        {
            let _ = child.kill().await;
        }
        process.alive.store(false, Ordering::Release);
        lock(&process.pending).clear();
    }
}

async fn keep_log(mut stderr: impl AsyncRead + Unpin, log: Arc<Mutex<VecDeque<String>>>) {
    let mut chunk = [0u8; 4096];
    let mut line: Vec<u8> = Vec::new();
    let push = |line: &mut Vec<u8>| {
        let text = String::from_utf8_lossy(line).trim_end().to_string();
        line.clear();
        if text.is_empty() {
            return;
        }
        let mut log = lock(&log);
        if log.len() == LOG_LINES {
            log.pop_front();
        }
        log.push_back(super::cut(&text, LOG_LINE_CHARS));
    };
    loop {
        let n = match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &chunk[..n] {
            if b == b'\n' {
                push(&mut line);
            } else if line.len() < LOG_LINE_CHARS * 4 {
                line.push(b);
            }
        }
    }
    push(&mut line);
}

async fn read_messages(
    mut stdout: impl AsyncRead + Unpin,
    max: usize,
    pending: Pending,
    alive: Arc<AtomicBool>,
    ended: Arc<Mutex<Option<String>>>,
    out: mpsc::UnboundedSender<Out>,
    log: criogenai_rt::JoinHandle<()>,
) {
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    let why = 'read: loop {
        let n = match stdout.read(&mut chunk).await {
            Ok(0) => break "the server process exited".to_string(),
            Err(e) => break format!("reading from the server failed: {e}"),
            Ok(n) => n,
        };
        buffer.extend_from_slice(&chunk[..n]);
        while let Some(at) = buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=at).collect();
            if line.len() > max {
                break 'read format!("the server sent a message larger than {max} bytes");
            }
            handle(&line, &pending, &out);
        }
        if buffer.len() > max {
            break format!("the server sent a message larger than {max} bytes");
        }
    };
    // Let the log catch up (its last lines say why the server stopped).
    let _ = criogenai_rt::timeout(Duration::from_millis(500), log).await;
    *lock(&ended) = Some(why);
    alive.store(false, Ordering::Release);
    // Waiting callers see their channel closed.
    lock(&pending).clear();
    // Stop the writer (the process is killed when its handle is dropped).
    let _ = out.send(Out::Close);
}

fn handle(line: &[u8], pending: &Pending, out: &mpsc::UnboundedSender<Out>) {
    // Lines that are not JSON (a stray log line) are skipped.
    let Ok(message) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let Some(object) = message.as_object() else {
        return;
    };
    match (object.get("method"), object.get("id")) {
        // A request from the server: only `ping` is supported.
        (Some(method), Some(id)) => {
            let reply = if method.as_str() == Some("ping") {
                json!({"jsonrpc": "2.0", "id": id, "result": {}})
            } else {
                json!({"jsonrpc": "2.0", "id": id, "error": {
                    "code": rpc::METHOD_NOT_FOUND,
                    "message": "not supported by this client",
                }})
            };
            let mut line = serde_json::to_vec(&reply).unwrap_or_default();
            line.push(b'\n');
            let _ = out.send(Out::Line(line));
        }
        // A notification: nothing to do in this slice.
        (Some(_), None) => {}
        (None, Some(id)) => {
            if let Some(id) = id.as_i64()
                && let Some(waiting) = lock(pending).remove(&id)
            {
                let _ = waiting.send(message);
            }
        }
        (None, None) => {}
    }
}
