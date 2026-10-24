"""Internet tools for agents: web search (Tavily or Brave) and reading a page.

    from agentbuild import Agent, web

    agent = Agent("openai:gpt-5-mini", tools=[web.search(), web.fetch()])

``web.search()`` needs no arguments: it uses ``TAVILY_API_KEY`` if set, else
``BRAVE_API_KEY``, and says clearly what to set if neither is. The key is read
from the environment (or passed in), sent only in a request header over
``https``, and never appears in an error, a log or a tool result.

Results are data for the model, so they are bounded: a few results, short
snippets, and pages cut to a fixed size. ``fetch`` refuses addresses on private
networks (a model must not be able to reach your internal services through
it), does not follow redirects, and reads at most 500 KB.
"""

import ipaddress
import json
import os
import socket
import urllib.error
import urllib.parse
import urllib.request
from html.parser import HTMLParser
from typing import Any, Callable, Dict, List, Optional

from .tools import Tool, ToolError, tool

__all__ = ["search", "tavily_search", "brave_search", "fetch", "default_tools"]

TAVILY_URL = "https://api.tavily.com"
BRAVE_URL = "https://api.search.brave.com"
MAX_RESULTS = 10
SNIPPET_CHARS = 600
MAX_BODY = 1_000_000  # bytes read from a search API
MAX_PAGE_BYTES = 500_000
MAX_PAGE_CHARS = 8_000
TIMEOUT = 20.0


def _key(explicit: Optional[str], env: str, provider: str) -> str:
    key = explicit or os.environ.get(env)
    if not key:
        raise ToolError(f"{provider} is not set up: set the {env} environment variable (or pass api_key=)", retryable=False)
    return key


def _check_url(url: str) -> urllib.parse.SplitResult:
    """Credentials only travel over https (plain http only to this computer, for tests)."""
    parts = urllib.parse.urlsplit(url)
    host = parts.hostname or ""
    if parts.scheme == "https":
        return parts
    if parts.scheme == "http" and host in ("127.0.0.1", "localhost", "::1"):
        return parts
    raise ValueError(f"{url!r}: search APIs are called over https only")


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args: Any, **kwargs: Any) -> None:  # a redirect could carry the key elsewhere
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def _request(url: str, *, data: Optional[bytes], headers: Dict[str, str], limit: int) -> bytes:
    request = urllib.request.Request(url, data=data, headers=headers)
    try:
        with _OPENER.open(request, timeout=TIMEOUT) as response:
            body = response.read(limit + 1)
    except urllib.error.HTTPError as error:
        # The status is useful to the model; the response body might echo the request, so it is not shown.
        retryable = error.code in (408, 429) or error.code >= 500
        raise ToolError(f"search failed: HTTP {error.code}", retryable=retryable) from None
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise ToolError(f"search failed: {type(error).__name__}", retryable=True) from None
    if len(body) > limit:
        raise ToolError("search failed: the response was too large", retryable=False)
    return body


def _trim(text: Any, n: int = SNIPPET_CHARS) -> str:
    text = " ".join(str(text or "").split())
    return text if len(text) <= n else text[: n - 1] + "…"


def _results(items: Any, title: str, url: str, snippet: str, limit: int) -> List[Dict[str, str]]:
    out = []
    for item in items if isinstance(items, list) else []:
        if isinstance(item, dict) and isinstance(item.get(url), str):
            out.append({"title": _trim(item.get(title), 200), "url": item[url], "snippet": _trim(item.get(snippet))})
        if len(out) >= limit:
            break
    return out


def tavily_search(api_key: Optional[str] = None, *, max_results: int = 5, depth: str = "basic", base_url: str = TAVILY_URL) -> Tool:
    """A ``web_search`` tool backed by Tavily (``TAVILY_API_KEY``). ``depth``: ``"basic"`` or ``"advanced"``."""
    limit = max(1, min(int(max_results), MAX_RESULTS))
    if depth not in ("basic", "advanced"):
        raise ValueError("depth is 'basic' or 'advanced'")
    endpoint = _check_url(base_url.rstrip("/") + "/search").geturl()

    @tool(read_only=True, name="web_search")
    def web_search(query: str) -> dict:
        """Search the web for current information. Returns an answer (when available) and a few results with title, url and snippet."""
        key = _key(api_key, "TAVILY_API_KEY", "Tavily")
        payload = {"query": query[:400], "max_results": limit, "search_depth": depth, "include_answer": True}
        body = _request(
            endpoint,
            data=json.dumps(payload).encode(),
            headers={"content-type": "application/json", "authorization": f"Bearer {key}"},
            limit=MAX_BODY,
        )
        try:
            data = json.loads(body)
        except ValueError:
            raise ToolError("search failed: the response was not JSON", retryable=False) from None
        answer = data.get("answer") if isinstance(data, dict) else None
        return {
            "answer": _trim(answer) if isinstance(answer, str) else None,
            "results": _results(data.get("results") if isinstance(data, dict) else None, "title", "url", "content", limit),
        }

    return web_search


def brave_search(api_key: Optional[str] = None, *, max_results: int = 5, base_url: str = BRAVE_URL) -> Tool:
    """A ``web_search`` tool backed by Brave Search (``BRAVE_API_KEY``)."""
    limit = max(1, min(int(max_results), MAX_RESULTS))
    base = _check_url(base_url.rstrip("/") + "/res/v1/web/search").geturl()

    @tool(read_only=True, name="web_search")
    def web_search(query: str) -> dict:
        """Search the web for current information. Returns a few results with title, url and snippet."""
        key = _key(api_key, "BRAVE_API_KEY", "Brave Search")
        url = f"{base}?{urllib.parse.urlencode({'q': query[:400], 'count': limit})}"
        body = _request(url, data=None, headers={"accept": "application/json", "x-subscription-token": key}, limit=MAX_BODY)
        try:
            data = json.loads(body)
        except ValueError:
            raise ToolError("search failed: the response was not JSON", retryable=False) from None
        web = data.get("web") if isinstance(data, dict) else None
        return {"answer": None, "results": _results((web or {}).get("results"), "title", "url", "description", limit)}

    return web_search


def search(api_key: Optional[str] = None, *, provider: Optional[str] = None, max_results: int = 5) -> Tool:
    """A web search tool with no setup: Tavily if ``TAVILY_API_KEY`` is set, else Brave if ``BRAVE_API_KEY`` is.
    ``provider="tavily"`` or ``"brave"`` chooses explicitly."""
    chosen = provider or ("tavily" if os.environ.get("TAVILY_API_KEY") else "brave" if os.environ.get("BRAVE_API_KEY") else None)
    if chosen is None and api_key is None:
        raise ValueError("web search needs a key: set TAVILY_API_KEY (tavily.com) or BRAVE_API_KEY, or pass provider= and api_key=")
    if chosen is None or chosen == "tavily":
        return tavily_search(api_key, max_results=max_results)
    if chosen == "brave":
        return brave_search(api_key, max_results=max_results)
    raise ValueError("provider is 'tavily' or 'brave'")


def default_tools() -> List[Tool]:
    """What a researcher gets when you give it no tools: web search if a key is set, else nothing."""
    try:
        return [search(), fetch()]
    except ValueError:
        return []


# ---------------------------------------------------------------- reading pages


class _Text(HTMLParser):
    SKIP = {"script", "style", "noscript", "svg", "head", "template"}

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.parts: List[str] = []
        self.skip = 0

    def handle_starttag(self, tag: str, attrs: Any) -> None:
        if tag in self.SKIP:
            self.skip += 1
        elif tag in ("p", "br", "div", "li", "h1", "h2", "h3", "h4", "tr"):
            self.parts.append("\n")

    def handle_endtag(self, tag: str) -> None:
        if tag in self.SKIP and self.skip:
            self.skip -= 1

    def handle_data(self, data: str) -> None:
        if not self.skip:
            self.parts.append(data)


def _public_address(host: str, allow_private: bool) -> None:
    """Refuses a host that resolves to a private, loopback or link-local address."""
    try:
        infos = socket.getaddrinfo(host, None)
    except OSError:
        raise ToolError(f"could not find the host {host!r}", retryable=False) from None
    for info in infos:
        address = ipaddress.ip_address(info[4][0])
        if not allow_private and (address.is_private or address.is_loopback or address.is_link_local or address.is_reserved or address.is_multicast):
            raise ToolError("that address is on a private network; only public web pages can be read", retryable=False)


def fetch(*, allow_private: bool = False, max_chars: int = MAX_PAGE_CHARS) -> Tool:
    """A ``read_page`` tool: fetches a public web page and returns its text (cut to ``max_chars``).
    Private-network addresses are refused unless ``allow_private=True`` (tests, or on purpose)."""
    cap = max(200, min(int(max_chars), 50_000))

    @tool(read_only=True, name="read_page")
    def read_page(url: str) -> dict:
        """Read a public web page (an http or https URL) and return its text."""
        parts = urllib.parse.urlsplit(url)
        if parts.scheme not in ("http", "https") or not parts.hostname:
            raise ToolError("give a full http or https URL", retryable=False)
        _public_address(parts.hostname, allow_private)
        body = _request(url, data=None, headers={"user-agent": "agentbuild-web/0.1", "accept": "text/html,text/plain"}, limit=MAX_PAGE_BYTES)
        parser = _Text()
        parser.feed(body.decode("utf-8", errors="replace"))
        text = "\n".join(line.strip() for line in "".join(parser.parts).splitlines() if line.strip())
        return {"url": url, "text": text[:cap], "truncated": len(text) > cap}

    return read_page
