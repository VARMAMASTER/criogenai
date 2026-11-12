//! A fake OTLP collector on localhost: answers each connection with the next
//! scripted status, and records every request. No network access.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One received export request.
#[derive(Debug, Clone)]
pub struct Request {
    pub head: String,
    pub body: serde_json::Value,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|line| {
            let (n, v) = line.split_once(':')?;
            n.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }

    /// Every span in the request.
    pub fn spans(&self) -> Vec<serde_json::Value> {
        self.body["resourceSpans"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|r| r["scopeSpans"].as_array().cloned().unwrap_or_default())
            .flat_map(|s| s["spans"].as_array().cloned().unwrap_or_default())
            .collect()
    }
}

/// Serves until dropped. `statuses` are used in order, then 200 forever.
pub struct Collector {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

impl Collector {
    pub async fn start(statuses: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/traces", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        tokio::spawn(async move {
            let mut statuses = statuses.into_iter();
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let status = statuses.next().unwrap_or(200);
                let mut buffer = Vec::new();
                let mut chunk = vec![0u8; 64 * 1024];
                let head_end = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break None;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break Some(i + 4);
                    }
                };
                let Some(head_end) = head_end else { continue };
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
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                let body = serde_json::from_slice(&buffer[head_end..]).unwrap_or_default();
                recorded.lock().unwrap().push(Request { head, body });
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}"
                );
                let _ = socket.write_all(reply.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        Self { url, requests }
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// Spans across all successful requests.
    pub fn spans(&self) -> Vec<serde_json::Value> {
        self.requests().iter().flat_map(Request::spans).collect()
    }
}
