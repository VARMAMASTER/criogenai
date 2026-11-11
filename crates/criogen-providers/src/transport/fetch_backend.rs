//! The browser's HTTP backend (design 0021): the host's `fetch` (browsers,
//! Web Workers, Node 18+, Deno, edge runtimes).
//!
//! Same rules as on servers: redirects are never followed (`redirect:
//! "manual"`, then refused by the caller), a body larger than the limit is
//! refused before it is copied into WebAssembly memory, and a request whose
//! future is dropped (a timeout, a cancelled run) is aborted, so the
//! download stops and the provider stops generating.

use criogen_rt::SendWrapper;
use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortController, Headers, ReadableStreamDefaultReader, Request, RequestInit, RequestRedirect,
    ResponseType,
};

use crate::error::{Dispatch, ModelErrorKind};
use crate::transport::{HttpConfig, WireRequest};

#[wasm_bindgen]
extern "C" {
    /// The global `fetch`, in every host (no `window` needed).
    #[wasm_bindgen(js_name = fetch, catch)]
    fn global_fetch(request: &Request) -> Result<js_sys::Promise, JsValue>;
}

/// `fetch` keeps no state of ours: the host pools connections.
#[derive(Clone)]
pub(super) struct Client;

impl Client {
    pub(super) fn new(_config: &HttpConfig) -> Result<Self, String> {
        Ok(Client)
    }

    pub(super) async fn post(&self, request: &WireRequest) -> Result<Response, Error> {
        let (method, url, headers, body) = (
            request.method,
            request.url.clone(),
            request.headers.clone(),
            request.body.clone(),
        );
        // JavaScript values are not `Send`; WebAssembly runs on one thread.
        SendWrapper::new(fetch(method, url, headers, body)).await
    }
}

async fn fetch(
    method: &'static str,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Result<Response, Error> {
    let init = RequestInit::new();
    init.set_method(method);
    init.set_redirect(RequestRedirect::Manual);
    let js_headers = Headers::new().map_err(Error::setup)?;
    for (name, value) in &headers {
        js_headers.set(name, value).map_err(Error::setup)?;
    }
    init.set_headers(&js_headers);
    if method != "GET" {
        init.set_body(&Uint8Array::from(body.as_slice()));
    }
    let controller = AbortController::new().map_err(Error::setup)?;
    init.set_signal(Some(&controller.signal()));
    // Aborts the request if this future is dropped before it answers.
    let abort = AbortOnDrop(controller);
    let request = Request::new_with_str_and_init(&url, &init).map_err(Error::setup)?;
    let promise = global_fetch(&request).map_err(Error::setup)?;
    let value = JsFuture::from(promise).await.map_err(Error::network)?;
    let response = value
        .dyn_into::<web_sys::Response>()
        .map_err(|_| Error::protocol("fetch answered something that is not a Response"))?;
    Ok(Response {
        response: SendWrapper::new(response),
        reader: None,
        _abort: SendWrapper::new(abort),
    })
}

/// Aborts the request when dropped: a body dropped half-read stops the
/// download (after the last chunk, aborting does nothing).
struct AbortOnDrop(AbortController);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(super) struct Response {
    response: SendWrapper<web_sys::Response>,
    reader: Option<SendWrapper<ReadableStreamDefaultReader>>,
    _abort: SendWrapper<AbortOnDrop>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status())
            .finish_non_exhaustive()
    }
}

impl Response {
    pub(super) fn status(&self) -> u16 {
        self.response.status()
    }

    /// A redirect, which `redirect: "manual"` did not follow. Browsers show
    /// it as an opaque response with status 0; other hosts as a 3xx.
    pub(super) fn is_redirect(&self) -> bool {
        self.response.type_() == ResponseType::Opaqueredirect || (300..400).contains(&self.status())
    }

    /// The headers the host lets a page read (CORS hides the rest, so
    /// `Retry-After` may be missing; classification then uses the status).
    pub(super) fn headers(&self) -> Vec<(String, String)> {
        let Ok(Some(entries)) = js_sys::try_iter(&self.response.headers()) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let pair = entry.dyn_into::<js_sys::Array>().ok()?;
                Some((pair.get(0).as_string()?, pair.get(1).as_string()?))
            })
            .collect()
    }

    /// The next chunk, refused before it is copied if it is larger than
    /// `allowed`.
    pub(super) async fn chunk(&mut self, allowed: usize) -> Result<Option<Vec<u8>>, Error> {
        if self.reader.is_none() {
            let Some(stream) = self.response.body() else {
                return Ok(None);
            };
            let reader = stream
                .get_reader()
                .unchecked_into::<ReadableStreamDefaultReader>();
            self.reader = Some(SendWrapper::new(reader));
        }
        let Some(reader) = self.reader.as_ref() else {
            return Ok(None);
        };
        let read = SendWrapper::new(JsFuture::from(reader.read()));
        let result = read.await.map_err(Error::network)?;
        let done = Reflect::get(&result, &JsValue::from_str("done"))
            .ok()
            .and_then(|done| done.as_bool())
            .unwrap_or(true);
        if done {
            return Ok(None);
        }
        let value = Reflect::get(&result, &JsValue::from_str("value")).map_err(Error::network)?;
        let bytes = value
            .dyn_into::<Uint8Array>()
            .map_err(|_| Error::protocol("a body chunk that is not bytes"))?;
        if bytes.length() as usize > allowed {
            return Err(Error::too_large());
        }
        Ok(Some(bytes.to_vec()))
    }
}

enum ErrorKind {
    /// The request could not be built: nothing was sent.
    Setup,
    /// `fetch` failed: the network, or the browser refused the response
    /// (CORS). Whether the server got the request is unknown.
    Network,
    /// Not what `fetch` promises.
    Protocol,
    /// Over `max_response_bytes`.
    TooLarge,
}

/// A request that failed, with what it tells about how far it got.
pub(super) struct Error {
    kind: ErrorKind,
    text: String,
}

impl Error {
    fn setup(value: JsValue) -> Self {
        Self {
            kind: ErrorKind::Setup,
            text: format!("the request could not be made: {}", message(&value)),
        }
    }

    fn network(value: JsValue) -> Self {
        Self {
            kind: ErrorKind::Network,
            text: format!(
                "the request failed before an answer arrived ({}): the network, or a server \
                 that does not accept calls from a browser page (CORS)",
                message(&value)
            ),
        }
    }

    fn protocol(text: &str) -> Self {
        Self {
            kind: ErrorKind::Protocol,
            text: text.to_string(),
        }
    }

    fn too_large() -> Self {
        Self {
            kind: ErrorKind::TooLarge,
            text: String::new(),
        }
    }

    pub(super) fn classify(&self, dispatch: Dispatch) -> (ModelErrorKind, Dispatch) {
        match self.kind {
            ErrorKind::Setup => (ModelErrorKind::InvalidRequest, Dispatch::NotSent),
            ErrorKind::Network => (ModelErrorKind::Network, dispatch),
            ErrorKind::Protocol | ErrorKind::TooLarge => {
                (ModelErrorKind::InvalidResponse, dispatch)
            }
        }
    }

    pub(super) fn describe(&self) -> String {
        self.text.clone()
    }

    pub(super) fn is_too_large(&self) -> bool {
        matches!(self.kind, ErrorKind::TooLarge)
    }
}

/// A thrown JavaScript value as text (an `Error`'s message and name).
fn message(value: &JsValue) -> String {
    if let Some(error) = value.dyn_ref::<js_sys::Error>() {
        let name = String::from(error.name());
        let text = String::from(error.message());
        return if name.is_empty() || name == "Error" {
            text
        } else {
            format!("{name}: {text}")
        };
    }
    value
        .as_string()
        .unwrap_or_else(|| "an unknown error".to_string())
}
