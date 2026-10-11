//! A scripted local HTTP server for transport and protocol tests. It serves
//! one connection, records the request, then follows a script of writes and
//! pauses. No network access, no external services.

#![allow(dead_code)]

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// What the server does after reading the request.
pub enum Step {
    /// Write these bytes.
    Send(Vec<u8>),
    /// Pause.
    Wait(Duration),
}

impl Step {
    pub fn send(bytes: impl AsRef<[u8]>) -> Self {
        Self::Send(bytes.as_ref().to_vec())
    }
}

/// The request as the server received it.
pub struct Received {
    pub head: String,
    pub body: Vec<u8>,
}

impl Received {
    /// The body parsed as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }

    /// A header value (name compared case-insensitively).
    pub fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|line| {
            let (n, v) = line.split_once(':')?;
            n.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }
}

/// Response head for a streamed (close-delimited) 200.
pub fn sse_head() -> Step {
    Step::send("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
}

/// A complete JSON response with this status.
pub fn json_response(status: u16, body: &str) -> Step {
    Step::Send(
        format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes(),
    )
}

/// One SSE event with JSON data.
pub fn sse_data(data: &str) -> Step {
    Step::Send(format!("data: {data}\n\n").into_bytes())
}

/// Serves one connection with `script`. Returns the base URL
/// (`http://127.0.0.1:<port>/v1`) and the received request.
pub async fn serve(script: Vec<Step>) -> (String, JoinHandle<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        let head_end = loop {
            let n = socket.read(&mut chunk).await.unwrap();
            buffer.extend_from_slice(&chunk[..n]);
            if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
        let length = head
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        while buffer.len() < head_end + length {
            let n = socket.read(&mut chunk).await.unwrap();
            buffer.extend_from_slice(&chunk[..n]);
        }
        let body = buffer[head_end..head_end + length].to_vec();
        for step in script {
            match step {
                Step::Send(bytes) => {
                    if socket.write_all(&bytes).await.is_err() {
                        break; // the client gave up
                    }
                    let _ = socket.flush().await;
                }
                Step::Wait(d) => tokio::time::sleep(d).await,
            }
        }
        Received { head, body }
    });
    (base, handle)
}
