//! The servers' HTTP backend: reqwest (rustls, HTTP/2, system proxies).

use crate::error::{Dispatch, ModelErrorKind};
use crate::transport::{HttpConfig, WireRequest};

/// A connection pool; clones share it.
#[derive(Clone)]
pub(super) struct Client(reqwest::Client);

impl Client {
    pub(super) fn new(config: &HttpConfig) -> Result<Self, String> {
        reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map(Self)
            .map_err(|err| describe(&err))
    }

    pub(super) async fn post(&self, request: &WireRequest) -> Result<Response, Error> {
        let mut builder = self.0.post(&request.url).body(request.body.clone());
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        builder.send().await.map(Response).map_err(Error::from)
    }
}

#[derive(Debug)]
pub(super) struct Response(reqwest::Response);

impl Response {
    pub(super) fn status(&self) -> u16 {
        self.0.status().as_u16()
    }

    pub(super) fn is_redirect(&self) -> bool {
        (300..400).contains(&self.status())
    }

    pub(super) fn headers(&self) -> Vec<(String, String)> {
        self.0
            .headers()
            .iter()
            .filter_map(|(n, v)| Some((n.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect()
    }

    /// The next chunk. `_allowed` is checked by the caller once the chunk
    /// arrived (reqwest has already read it into memory).
    pub(super) async fn chunk(&mut self, _allowed: usize) -> Result<Option<Vec<u8>>, Error> {
        self.0
            .chunk()
            .await
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
            .map_err(Error::from)
    }
}

/// A request that failed, with what it tells about how far it got.
pub(super) struct Error {
    connect: bool,
    timeout: bool,
    text: String,
}

impl From<reqwest::Error> for Error {
    fn from(err: reqwest::Error) -> Self {
        Self {
            connect: err.is_connect(),
            timeout: err.is_timeout(),
            // Without its URL, which may carry a key.
            text: describe(&err.without_url()),
        }
    }
}

impl Error {
    pub(super) fn classify(&self, dispatch: Dispatch) -> (ModelErrorKind, Dispatch) {
        if self.connect {
            // Never reached the server (DNS, refused, TLS): safe to retry.
            let kind = if self.timeout {
                ModelErrorKind::Timeout
            } else {
                ModelErrorKind::Network
            };
            (kind, Dispatch::NotSent)
        } else if self.timeout {
            (ModelErrorKind::Timeout, dispatch)
        } else {
            (ModelErrorKind::Network, dispatch)
        }
    }

    pub(super) fn describe(&self) -> String {
        self.text.clone()
    }

    pub(super) fn is_too_large(&self) -> bool {
        false
    }
}

/// The error and its causes (call it on an error without its URL, which may
/// carry a key).
fn describe(err: &reqwest::Error) -> String {
    let mut text = err.to_string();
    let mut source = std::error::Error::source(err);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
