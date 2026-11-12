# 0020: Kaggle Model Proxy as a provider

Status: approved by the user's request (2026-11-01). Built in Rust
(`criogenai-providers`), exposed in Python, TypeScript and the gateway config.

## Why

A Kaggle account comes with an AI-model allowance (at the time of writing
$10 a day and $100 a month) that pays for hosted models through Kaggle's
**Model Proxy**. It is good for prototypes, tests and MVPs. Agents should be
able to use it like any other provider: `"kaggle:google/gemini-3.6-flash"`.

## What Kaggle offers (checked live, 2026-11-01)

- `POST https://api.kaggle.com/v1/models.ModelProxyApiService/CreateDefaultModelProxyToken`
  with `Authorization: Bearer <Kaggle API token>` and body `{}` returns
  `{ "baseUri", "token", "expiryTime" }`. The proxy token lives about an hour.
- `POST {baseUri}/openapi/chat/completions` with `Authorization: Bearer
  <proxy token>` speaks the OpenAI Chat Completions format: streaming with
  usage, tool calls, `max_tokens` and `max_completion_tokens` all work.
  `temperature` is refused by some models (Claude Sonnet 5), accepted by
  others; CriogenAI only sends it when asked.
- A model the account may not use answers `404 not_found_error`, uncharged.
  There is no model-list endpoint.
- `GetModelProxyQuotas` (same auth as minting) reports the daily and monthly
  spend.

## Design

`KaggleModel` implements `Model`. It holds the Kaggle API token (never the
proxy URL in advance) and mints a proxy token on first use, then:

- **Renews before expiry**: at `expiryTime − 5 min`, and never later than
  50 minutes after minting (so an unreadable expiry still renews).
- **Renews once on `auth`**: a 401 from the proxy (token revoked or expired
  early) mints a new token and retries the call once. A streamed call is
  retried only if no delta was emitted.
- **Concurrency**: the session is behind a `std::sync::Mutex` held only to
  read or replace it, never across an `await`. Two calls that find it stale
  may both mint; the later one wins (harmless).
- **Pre-minted tokens**: `KaggleModel::from_proxy(url, token, model)` uses a
  proxy token as is (a Kaggle notebook that sets `MODEL_PROXY_URL` and
  `MODEL_PROXY_API_KEY`); it cannot renew.
- **Credentials from the environment** (`from_env`), in order:
  `MODEL_PROXY_URL` + `MODEL_PROXY_API_KEY`; `KAGGLE_API_TOKEN`; the file
  `access_token` in `KAGGLE_CONFIG_DIR` or `~/.kaggle` (what the Kaggle CLI
  writes). The file is read up to 4 KB.

The proxy call itself is an ordinary `ProviderModel` on an OpenAI-compatible
`Endpoint` named `kaggle`, so retries, fallbacks, streaming, tools and error
classification behave exactly as for other providers.

## Rules this keeps

- Rule 10: neither token appears in `Debug`, an error or a log; both are
  redacted from error text. Credentials are refused over plain `http://` to
  another machine (the minting URL is checked the same way as endpoints).
- Rule 11: the minting response goes through the bounded transport; its
  fields are data (an empty or missing token is an `invalid_response`).
- Rule 7: tests use a scripted local server for both Kaggle's API and the
  proxy, never the network. A manual live check is in `tests/live`.

## Not in this slice

Kaggle's Gemini-native `/genai` route, quota checks before a call, and a model
list (Kaggle has no endpoint for it).
