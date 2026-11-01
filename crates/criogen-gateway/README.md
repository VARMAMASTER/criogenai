# criogen-gateway

An OpenAI-compatible LLM gateway on [Criogen](https://github.com/VARMAMASTER/criogen)'s
provider layer (design 0006 §7, §12). Any OpenAI client (the official SDKs,
curl, Criogen's own) talks to it; each *route* maps to one or more
upstream models in any protocol Criogen speaks: OpenAI, Anthropic,
Gemini, OpenRouter, Ollama, vLLM and other OpenAI-compatible servers.

- `POST /v1/chat/completions` (streaming and not), `GET /v1/models`,
  `GET /health`
- fallback to the route's next model on rate limits, overload, outages
  and exhausted quota (a stream only before anything was sent)
- virtual keys per route (kept as SHA-256 digests); without keys the
  binary only listens on loopback unless `--allow-open`
- a usage record per request (route, model, tokens, latency, outcome; no
  content) on a channel that never slows requests
- the upstream call stops when the client goes away

```sh
OPENAI_API_KEY=… GATEWAY_KEY_TEAM_A=… \
  cargo run -p criogen-gateway -- --config examples/gateway.toml --log-usage
curl http://127.0.0.1:4000/v1/chat/completions -H "Authorization: Bearer $GATEWAY_KEY_TEAM_A" \
  -d '{"model": "support", "messages": [{"role": "user", "content": "Hello"}]}'
```

Or in your own server: `Gateway::builder().route("support", model).build()?.router()`
is an axum `Router`.

**Status:** first slice. Not yet: the relay fast path, `/v1/messages`
(Anthropic) and Responses inbound, budgets, rate limits, sticky routing,
benchmarks against other gateways.
