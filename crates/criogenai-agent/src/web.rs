//! Internet tools for agents: web search (Tavily or Brave) and reading a
//! page (design 0019). Behind the `web` cargo feature.
//!
//! ```no_run
//! use std::sync::Arc;
//! use criogenai_agent::{Agent, web};
//! # fn model() -> criogenai_providers::FakeModel { criogenai_providers::FakeModel::new() }
//!
//! let agent = Agent::builder(model())
//!     .tool(web::search().unwrap()) // TAVILY_API_KEY, else BRAVE_API_KEY
//!     .tool(web::fetch())
//!     .build()
//!     .unwrap();
//! ```
//!
//! [`search`] needs no arguments: it uses `TAVILY_API_KEY` if set, else
//! `BRAVE_API_KEY`, and says what to set if neither is. The key is read from
//! the environment (or passed in), sent only in a request header over
//! `https` (plain `http` only to this computer, for tests), and never
//! appears in an error, a log or a tool result.
//!
//! Results are data for the model, so they are bounded: a few results, short
//! snippets, and pages cut to a fixed size. [`fetch`] refuses addresses on
//! private networks (a model must not be able to reach your internal
//! services through it), pins the connection to the address it checked (so a
//! DNS answer cannot change between the check and the request), does not
//! follow redirects, and reads at most 500 KB.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use criogenai_providers::{BoxFuture, ToolAnnotations, ToolSpec};
use serde_json::{Map, Value, json};

use crate::tool::{Tool, ToolContext, ToolError, ToolOutput};

/// Tavily's API.
pub const TAVILY_URL: &str = "https://api.tavily.com";
/// Brave Search's API.
pub const BRAVE_URL: &str = "https://api.search.brave.com";
/// The most results a search returns, whatever is asked for.
pub const MAX_RESULTS: usize = 10;
/// A result's snippet is cut to this many characters.
pub const SNIPPET_CHARS: usize = 600;
/// Bytes read from a search API.
pub const MAX_BODY: usize = 1_000_000;
/// Bytes read from a page.
pub const MAX_PAGE_BYTES: usize = 500_000;
/// Characters of a page's text returned by default.
pub const MAX_PAGE_CHARS: usize = 8_000;
/// The longest a request may take.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Why a web tool could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WebError {
    /// No search key in the environment.
    #[error(
        "web search needs a key: set TAVILY_API_KEY (tavily.com) or BRAVE_API_KEY, \
         or build a tool with tavily_search().api_key(..) or brave_search().api_key(..)"
    )]
    NoKey,
    /// The base URL is not usable: it must be `https` (plain `http` only to
    /// this computer).
    #[error("search APIs are called over https only (plain http only to this computer)")]
    InsecureUrl,
    /// The base URL does not parse.
    #[error("the base URL is not a valid URL")]
    InvalidUrl,
}

/// Which search service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchProvider {
    /// Tavily (`TAVILY_API_KEY`).
    Tavily,
    /// Brave Search (`BRAVE_API_KEY`).
    Brave,
}

/// Tavily's search depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchDepth {
    /// Fast.
    Basic,
    /// Slower, more thorough.
    Advanced,
}

impl SearchProvider {
    fn env(self) -> &'static str {
        match self {
            Self::Tavily => "TAVILY_API_KEY",
            Self::Brave => "BRAVE_API_KEY",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Tavily => "Tavily",
            Self::Brave => "Brave Search",
        }
    }
}

fn env_key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|k| !k.is_empty())
}

fn check_url(url: &str) -> Result<reqwest::Url, WebError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| WebError::InvalidUrl)?;
    let local = matches!(
        parsed.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    );
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if local => Ok(parsed),
        _ => Err(WebError::InsecureUrl),
    }
}

fn client() -> Option<reqwest::Client> {
    // A redirect could carry the key (or the model's request) elsewhere.
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .build()
        .ok()
}

/// Collapses whitespace and cuts to `n` characters (ending in an ellipsis).
fn trim(text: &str, n: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= n {
        text
    } else {
        let mut cut: String = text.chars().take(n.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

fn retryable_status(code: u16) -> bool {
    code == 408 || code == 429 || code >= 500
}

/// What went wrong, in words that cannot hold the URL or the key.
fn request_failure(prefix: &str, error: &reqwest::Error) -> ToolError {
    let what = if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "request failed"
    };
    ToolError::new(format!("{prefix}: {what}")).retryable()
}

/// Reads a body in chunks, refusing more than `limit` bytes.
async fn read_body(
    mut response: reqwest::Response,
    limit: usize,
    prefix: &str,
) -> Result<Vec<u8>, ToolError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| request_failure(prefix, &e))?
    {
        if body.len() + chunk.len() > limit {
            return Err(ToolError::new(format!(
                "{prefix}: the response was too large"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

// -------------------------------------------------------------------- search

/// A `web_search` tool. Build it with [`tavily_search`], [`brave_search`] or
/// [`search`].
#[derive(Clone)]
pub struct SearchTool {
    provider: SearchProvider,
    api_key: Option<String>,
    limit: usize,
    depth: SearchDepth,
    endpoint: reqwest::Url,
    spec: ToolSpec,
    client: Option<reqwest::Client>,
}

impl std::fmt::Debug for SearchTool {
    /// The key is never shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchTool")
            .field("provider", &self.provider)
            .field("limit", &self.limit)
            .field("api_key", &self.api_key.as_ref().map(|_| "<set>"))
            .finish_non_exhaustive()
    }
}

impl SearchTool {
    fn new(provider: SearchProvider) -> Self {
        let (base, path, about) = match provider {
            SearchProvider::Tavily => (
                TAVILY_URL,
                "/search",
                "Search the web for current information. Returns an answer (when available) and a few results with title, url and snippet.",
            ),
            SearchProvider::Brave => (
                BRAVE_URL,
                "/res/v1/web/search",
                "Search the web for current information. Returns a few results with title, url and snippet.",
            ),
        };
        let spec = ToolSpec::new(
            "web_search",
            about,
            json!({
                "type": "object",
                "properties": {"query": {"type": "string", "description": "What to search for."}},
                "required": ["query"],
            }),
        )
        .with_annotations(ToolAnnotations::read_only());
        Self {
            provider,
            api_key: None,
            limit: 5,
            depth: SearchDepth::Basic,
            endpoint: reqwest::Url::parse(&format!("{base}{path}")).expect("a valid built-in URL"),
            spec,
            client: client(),
        }
    }

    /// The key to use (default: the provider's environment variable, read at
    /// each call).
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Results returned: 1 to [`MAX_RESULTS`] (default 5).
    pub fn max_results(mut self, n: usize) -> Self {
        self.limit = n.clamp(1, MAX_RESULTS);
        self
    }

    /// Tavily's search depth (default basic).
    pub fn depth(mut self, depth: SearchDepth) -> Self {
        self.depth = depth;
        self
    }

    /// Another server for the same API (a proxy, a test server). Must be
    /// `https`, or `http` to this computer: the key travels there.
    pub fn base_url(mut self, base: &str) -> Result<Self, WebError> {
        let path = match self.provider {
            SearchProvider::Tavily => "/search",
            SearchProvider::Brave => "/res/v1/web/search",
        };
        self.endpoint = check_url(&format!("{}{path}", base.trim_end_matches('/')))?;
        Ok(self)
    }

    async fn search(&self, query: &str) -> Result<ToolOutput, ToolError> {
        const PREFIX: &str = "search failed";
        let provider = self.provider;
        let key = self
            .api_key
            .clone()
            .or_else(|| env_key(provider.env()))
            .ok_or_else(|| {
                ToolError::new(format!(
                    "{} is not set up: set the {} environment variable (or pass api_key)",
                    provider.label(),
                    provider.env()
                ))
            })?;
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| ToolError::new("search failed: could not create the HTTP client"))?;
        let query: String = query.chars().take(400).collect();
        let request = match provider {
            SearchProvider::Tavily => client
                .post(self.endpoint.clone())
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {key}"))
                .body(
                    json!({
                        "query": query,
                        "max_results": self.limit,
                        "search_depth": match self.depth {
                            SearchDepth::Basic => "basic",
                            SearchDepth::Advanced => "advanced",
                        },
                        "include_answer": true,
                    })
                    .to_string(),
                ),
            SearchProvider::Brave => {
                let mut url = self.endpoint.clone();
                url.query_pairs_mut()
                    .append_pair("q", &query)
                    .append_pair("count", &self.limit.to_string());
                client
                    .get(url)
                    .header("accept", "application/json")
                    .header("x-subscription-token", key)
            }
        };
        let response = request
            .send()
            .await
            .map_err(|e| request_failure(PREFIX, &e))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            // The status helps the model; the body might echo the request.
            let error = ToolError::new(format!("{PREFIX}: HTTP {status}"));
            return Err(if retryable_status(status) {
                error.retryable()
            } else {
                error
            });
        }
        let body = read_body(response, MAX_BODY, PREFIX).await?;
        let data: Value = serde_json::from_slice(&body)
            .map_err(|_| ToolError::new(format!("{PREFIX}: the response was not JSON")))?;
        let (answer, items, title, url, snippet) = match provider {
            SearchProvider::Tavily => (
                data.get("answer")
                    .and_then(Value::as_str)
                    .map(|a| trim(a, SNIPPET_CHARS)),
                data.get("results"),
                "title",
                "url",
                "content",
            ),
            SearchProvider::Brave => (
                None,
                data.get("web").and_then(|w| w.get("results")),
                "title",
                "url",
                "description",
            ),
        };
        let results: Vec<Value> = items
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let link = item.get(url)?.as_str()?;
                        let text = |key: &str, n: usize| {
                            trim(item.get(key).and_then(Value::as_str).unwrap_or_default(), n)
                        };
                        Some(json!({
                            "title": text(title, 200),
                            "url": link,
                            "snippet": text(snippet, SNIPPET_CHARS),
                        }))
                    })
                    .take(self.limit)
                    .collect()
            })
            .unwrap_or_default();
        Ok(ToolOutput::Json(
            json!({"answer": answer, "results": results}),
        ))
    }
}

impl Tool for SearchTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn call(
        &self,
        args: Map<String, Value>,
        _ctx: ToolContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::invalid_arguments("query must be a string"))?;
            self.search(query).await
        })
    }
}

/// A `web_search` tool backed by Tavily (`TAVILY_API_KEY`).
pub fn tavily_search() -> SearchTool {
    SearchTool::new(SearchProvider::Tavily)
}

/// A `web_search` tool backed by Brave Search (`BRAVE_API_KEY`).
pub fn brave_search() -> SearchTool {
    SearchTool::new(SearchProvider::Brave)
}

/// A web search tool with no setup: Tavily if `TAVILY_API_KEY` is set, else
/// Brave if `BRAVE_API_KEY` is; [`WebError::NoKey`] says what to set if
/// neither is.
pub fn search() -> Result<SearchTool, WebError> {
    if env_key("TAVILY_API_KEY").is_some() {
        Ok(tavily_search())
    } else if env_key("BRAVE_API_KEY").is_some() {
        Ok(brave_search())
    } else {
        Err(WebError::NoKey)
    }
}

/// What a researcher gets when you give it no tools: web search and page
/// reading if a key is set, else nothing.
pub fn default_tools() -> Vec<Arc<dyn Tool>> {
    match search() {
        Ok(search) => vec![Arc::new(search), Arc::new(fetch())],
        Err(_) => Vec::new(),
    }
}

// ------------------------------------------------------------- reading pages

/// A `read_page` tool. Build it with [`fetch`].
#[derive(Debug, Clone)]
pub struct FetchTool {
    allow_private: bool,
    max_chars: usize,
    spec: ToolSpec,
}

/// A `read_page` tool: fetches a public web page and returns its text (cut
/// to 8,000 characters by default). Private-network addresses are refused
/// unless [`FetchTool::allow_private`] (tests, or on purpose).
pub fn fetch() -> FetchTool {
    let spec = ToolSpec::new(
        "read_page",
        "Read a public web page (an http or https URL) and return its text.",
        json!({
            "type": "object",
            "properties": {"url": {"type": "string", "description": "The full URL of the page."}},
            "required": ["url"],
        }),
    )
    .with_annotations(ToolAnnotations::read_only());
    FetchTool {
        allow_private: false,
        max_chars: MAX_PAGE_CHARS,
        spec,
    }
}

impl FetchTool {
    /// Allows pages on private, loopback and link-local addresses. Off by
    /// default: a model must not reach your internal services through this
    /// tool.
    pub fn allow_private(mut self, allow: bool) -> Self {
        self.allow_private = allow;
        self
    }

    /// Characters of text returned: 200 to 50,000 (default 8,000).
    pub fn max_chars(mut self, n: usize) -> Self {
        self.max_chars = n.clamp(200, 50_000);
        self
    }

    async fn read(&self, url: &str) -> Result<ToolOutput, ToolError> {
        const PREFIX: &str = "could not read the page";
        let parsed = reqwest::Url::parse(url)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
            .ok_or_else(|| ToolError::new("give a full http or https URL"))?;
        let host = parsed.host_str().unwrap_or_default().to_string();
        let port = parsed.port_or_known_default().unwrap_or(80);
        let literal: Option<IpAddr> = host.trim_matches(['[', ']']).parse().ok();
        let addrs: Vec<SocketAddr> = match literal {
            Some(ip) => vec![SocketAddr::new(ip, port)],
            None => tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| ToolError::new(format!("could not find the host {host:?}")))?
                .collect(),
        };
        if addrs.is_empty() {
            return Err(ToolError::new(format!("could not find the host {host:?}")));
        }
        if !self.allow_private && addrs.iter().any(|a| !is_public(a.ip())) {
            return Err(ToolError::new(
                "that address is on a private network; only public web pages can be read",
            ));
        }
        // Connect to the addresses that were checked, not to a second DNS answer.
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT);
        if literal.is_none() {
            builder = builder.resolve_to_addrs(&host, &addrs);
        }
        let client = builder
            .build()
            .map_err(|_| ToolError::new(format!("{PREFIX}: could not create the HTTP client")))?;
        let response = client
            .get(parsed)
            .header("user-agent", "criogenai-web/0.1")
            .header("accept", "text/html,text/plain")
            .send()
            .await
            .map_err(|e| request_failure(PREFIX, &e))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            return Err(ToolError::new(format!(
                "{PREFIX}: the page redirected (HTTP {status}); redirects are not followed"
            )));
        }
        if !(200..300).contains(&status) {
            let error = ToolError::new(format!("{PREFIX}: HTTP {status}"));
            return Err(if retryable_status(status) {
                error.retryable()
            } else {
                error
            });
        }
        let html = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|t| t.contains("html"));
        let body = read_body(response, MAX_PAGE_BYTES, PREFIX).await?;
        let raw = String::from_utf8_lossy(&body);
        let text = if html {
            html_to_text(&raw)
        } else {
            clean_lines(&raw)
        };
        let total = text.chars().count();
        let shown: String = text.chars().take(self.max_chars).collect();
        Ok(ToolOutput::Json(
            json!({"url": url, "text": shown, "truncated": total > self.max_chars}),
        ))
    }
}

impl Tool for FetchTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn call(
        &self,
        args: Map<String, Value>,
        _ctx: ToolContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let url = args
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::invalid_arguments("url must be a string"))?;
            self.read(url).await
        })
    }
}

/// `true` for an address on the public internet: not private, loopback,
/// link-local, shared, reserved, multicast or documentation space (IPv4, IPv6
/// and the IPv4 addresses embedded in IPv6 forms).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return public_v4(mapped);
            }
            let s = v6.segments();
            let embedded = |hi: u16, lo: u16| {
                public_v4(Ipv4Addr::new(
                    (hi >> 8) as u8,
                    hi as u8,
                    (lo >> 8) as u8,
                    lo as u8,
                ))
            };
            if v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link-local
                || (s[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
                || (s[0] == 0x2001 && (s[1] == 0 || s[1] == 0x0db8))
            // Teredo, documentation
            {
                return false;
            }
            if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
                return embedded(s[6], s[7]); // NAT64
            }
            if s[0] == 0x2002 {
                return embedded(s[1], s[2]); // 6to4
            }
            true
        }
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        || o[0] == 0
        || o[0] >= 240 // reserved
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // shared address space
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18)) // benchmarking
}

fn clean_lines(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The visible text of an HTML page: no scripts, styles or head, tags
/// removed, entities decoded, blocks on their own lines.
pub fn html_to_text(html: &str) -> String {
    const SKIP: [&str; 6] = ["script", "style", "noscript", "svg", "head", "template"];
    const BREAK: [&str; 9] = ["p", "br", "div", "li", "h1", "h2", "h3", "h4", "tr"];
    let chars: Vec<char> = html.chars().collect();
    let (mut out, mut skip, mut i) = (String::new(), 0usize, 0usize);
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            if chars[i..].starts_with(&['<', '!', '-', '-']) {
                let end = (i + 4..chars.len().saturating_sub(2))
                    .find(|&j| chars[j..].starts_with(&['-', '-', '>']));
                i = end.map_or(chars.len(), |j| j + 3);
                continue;
            }
            if chars
                .get(i + 1)
                .is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?'))
            {
                let (mut j, mut quote) = (i + 1, None::<char>);
                while j < chars.len() {
                    let d = chars[j];
                    match quote {
                        Some(q) if d == q => quote = None,
                        Some(_) => {}
                        None if d == '"' || d == '\'' => quote = Some(d),
                        None if d == '>' => break,
                        None => {}
                    }
                    j += 1;
                }
                let tag: String = chars[i + 1..j.min(chars.len())].iter().collect();
                let closing = tag.starts_with('/');
                let self_closing = tag.ends_with('/');
                let name: String = tag
                    .trim_start_matches('/')
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect::<String>()
                    .to_ascii_lowercase();
                if SKIP.contains(&name.as_str()) {
                    if closing {
                        skip = skip.saturating_sub(1);
                    } else if !self_closing {
                        skip += 1;
                    }
                } else if skip == 0 && BREAK.contains(&name.as_str()) {
                    out.push('\n');
                }
                i = j + 1;
                continue;
            }
        }
        if skip == 0 {
            if c == '&'
                && let Some((decoded, used)) = entity(&chars[i..])
            {
                out.push(decoded);
                i += used;
                continue;
            }
            out.push(c);
        }
        i += 1;
    }
    clean_lines(&out)
}

/// Decodes `&name;`, `&#123;` or `&#x1F;` at the start of `rest`: the
/// character and how many characters it took.
fn entity(rest: &[char]) -> Option<(char, usize)> {
    let end = rest.iter().take(12).position(|&c| c == ';')?;
    let name: String = rest[1..end].iter().collect();
    let decoded = match name.as_str() {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((decoded, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_on_private_networks_are_not_public() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.0.9",
            "172.16.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "240.0.0.1",
            "224.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
            "2001:db8::1",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn html_becomes_text_without_scripts_or_markup() {
        let page = "<html><head><title>t</title><script>evil()</script></head>\
                    <body><!-- note --><h1>Hello</h1><p>World &amp; &#65;&#x42; more</p>\
                    <style>p{}</style><a href=\"x>y\">link</a> 1 < 2</body></html>";
        let text = html_to_text(page);
        assert_eq!(text, "Hello\nWorld & AB more\nlink 1 < 2");
    }

    #[test]
    fn html_parsing_survives_broken_input() {
        for broken in [
            "<",
            "<a",
            "<a href=\"",
            "<!-- never closed",
            "&#9999999999;",
            "&",
            "<script>",
        ] {
            let _ = html_to_text(broken);
        }
    }

    #[test]
    fn urls_for_keys_must_be_https_or_this_computer() {
        assert!(check_url("https://search.example.com/x").is_ok());
        assert!(check_url("http://127.0.0.1:8080/x").is_ok());
        assert!(check_url("http://localhost/x").is_ok());
        assert_eq!(
            check_url("http://search.example.com").unwrap_err(),
            WebError::InsecureUrl
        );
    }

    #[test]
    fn a_key_is_not_shown_in_debug() {
        let tool = tavily_search().api_key("tvly-secret-value");
        assert!(!format!("{tool:?}").contains("secret"));
    }
}
