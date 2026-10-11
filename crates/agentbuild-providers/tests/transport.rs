//! The HTTP transport against a scripted local server (no network, no
//! external services). Real time with short timeouts: the paused clock does
//! not mix with real sockets.

#![cfg(feature = "http")]

use std::time::{Duration, Instant};

use agentbuild_providers::{
    Dispatch, HttpConfig, ModelErrorKind, SseParser, Transport, WireRequest,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// What the server does after reading the request.
enum Step {
    Send(&'static [u8]),
    Wait(Duration),
}

/// The request as the server received it.
struct Received {
    head: String,
    body: Vec<u8>,
}

/// Serves one connection with `script`, then closes it.
async fn serve(script: Vec<Step>) -> (String, JoinHandle<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/chat", listener.local_addr().unwrap());
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
                    if socket.write_all(bytes).await.is_err() {
                        break; // the client gave up
                    }
                    let _ = socket.flush().await;
                }
                Step::Wait(d) => tokio::time::sleep(d).await,
            }
        }
        Received { head, body }
    });
    (url, handle)
}

const OK_HEAD: &[u8] =
    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";

fn transport(config: HttpConfig) -> Transport {
    Transport::new(config).unwrap()
}

fn fast() -> HttpConfig {
    HttpConfig::default()
        .connect_timeout(Duration::from_secs(2))
        .idle_timeout(Duration::from_millis(300))
        .total_timeout(Duration::from_secs(5))
}

#[tokio::test]
async fn send_returns_the_body_and_sends_headers_and_body() {
    let (url, server) = serve(vec![Step::Send(
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
    )])
    .await;
    let request = WireRequest::post(url, br#"{"model":"m"}"#.to_vec())
        .header("authorization", "Bearer key-1")
        .header("content-type", "application/json");
    let body = transport(fast()).send(&request).await.unwrap();
    assert_eq!(body, br#"{"ok":true}"#);

    let received = server.await.unwrap();
    assert!(received.head.starts_with("POST /v1/chat HTTP/1.1"));
    assert!(
        received
            .head
            .to_ascii_lowercase()
            .contains("authorization: bearer key-1")
    );
    assert_eq!(received.body, br#"{"model":"m"}"#);
}

#[tokio::test]
async fn error_status_is_classified_with_retry_after_and_secrets_removed() {
    let (url, _server) = serve(vec![Step::Send(
        b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 3\r\nx-request-id: req_9\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"error\": {\"message\": \"limit for key my-secret-key reached\", \"type\": \"rate_limit_error\"}}",
    )])
    .await;
    let transport = transport(fast()).redacting(["my-secret-key".to_string()]);
    let err = transport
        .send(&WireRequest::post(url, b"{}".to_vec()))
        .await
        .unwrap_err();
    assert_eq!(err.kind, ModelErrorKind::RateLimited);
    assert_eq!(err.status, Some(429));
    assert_eq!(err.retry_after(), Some(Duration::from_secs(3)));
    assert_eq!(err.request_id.as_deref(), Some("req_9"));
    assert_eq!(err.dispatch, Dispatch::Sent);
    assert_eq!(err.message, "limit for key [redacted] reached");
}

#[tokio::test]
async fn refused_connection_was_not_sent() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/chat", listener.local_addr().unwrap());
    drop(listener);
    // Linux and macOS refuse at once (`network`); Windows retries the
    // connection until the connect timeout (`timeout`). Either way the
    // request never left, so it is safe to retry.
    let config = fast().connect_timeout(Duration::from_millis(500));
    let err = transport(config)
        .send(&WireRequest::post(url.clone(), b"{}".to_vec()))
        .await
        .unwrap_err();
    assert!(
        matches!(err.kind, ModelErrorKind::Network | ModelErrorKind::Timeout),
        "{err}"
    );
    assert_eq!(err.dispatch, Dispatch::NotSent, "safe to retry: {err}");
    assert!(
        !err.message.contains("127.0.0.1"),
        "no URL: {}",
        err.message
    );
}

#[tokio::test]
async fn no_response_hits_the_first_byte_timeout() {
    let (url, _server) = serve(vec![Step::Wait(Duration::from_secs(5))]).await;
    let config = fast().first_byte_timeout(Duration::from_millis(200));
    let started = Instant::now();
    let err = transport(config)
        .send(&WireRequest::post(url, b"{}".to_vec()))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(err.kind, ModelErrorKind::Timeout);
    assert_eq!(err.dispatch, Dispatch::Sent);
    assert_eq!(err.message, "no response within 200 ms");
}

#[tokio::test]
async fn stalled_stream_hits_the_idle_timeout() {
    let (url, _server) = serve(vec![
        Step::Send(OK_HEAD),
        Step::Send(b"data: one\n\n"),
        Step::Wait(Duration::from_secs(5)),
    ])
    .await;
    let mut body = transport(fast())
        .open(&WireRequest::post(url, b"{}".to_vec()))
        .await
        .unwrap();
    let first = body.next_chunk().await.unwrap().unwrap();
    assert_eq!(first, b"data: one\n\n");
    let started = Instant::now();
    let err = body.next_chunk().await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(err.kind, ModelErrorKind::Timeout);
    assert_eq!(err.dispatch, Dispatch::Streaming);
    assert_eq!(err.message, "no data arrived for 300 ms");
}

#[tokio::test]
async fn trickling_stream_hits_the_total_timeout() {
    let mut script = vec![Step::Send(OK_HEAD)];
    for _ in 0..40 {
        script.push(Step::Send(b": keep-alive\n\n"));
        script.push(Step::Wait(Duration::from_millis(100)));
    }
    let (url, _server) = serve(script).await;
    let config = fast().total_timeout(Duration::from_millis(600));
    let mut body = transport(config)
        .open(&WireRequest::post(url, b"{}".to_vec()))
        .await
        .unwrap();
    let err = loop {
        match body.next_chunk().await {
            Ok(Some(_)) => continue,
            Ok(None) => panic!("stream ended before the timeout"),
            Err(err) => break err,
        }
    };
    assert_eq!(err.kind, ModelErrorKind::Timeout);
    assert_eq!(err.message, "the response did not finish within 600 ms");
}

#[tokio::test]
async fn streamed_sse_survives_arbitrary_packet_boundaries() {
    let (url, _server) = serve(vec![
        Step::Send(OK_HEAD),
        Step::Send(b"data: {\"a\""),
        Step::Wait(Duration::from_millis(20)),
        Step::Send(b": 1}\r"),
        Step::Wait(Duration::from_millis(20)),
        Step::Send(b"\n\r\nevent: done\ndata: [DONE]\n\n"),
    ])
    .await;
    let mut body = transport(fast())
        .open(&WireRequest::post(url, b"{}".to_vec()))
        .await
        .unwrap();
    let mut parser = SseParser::new();
    let mut events = Vec::new();
    while let Some(chunk) = body.next_chunk().await.unwrap() {
        events.extend(parser.feed(&chunk));
    }
    let data: Vec<&str> = events.iter().map(|e| e.data.as_str()).collect();
    assert_eq!(data, ["{\"a\": 1}", "[DONE]"]);
    assert_eq!(events[1].event_type(), "done");
}
