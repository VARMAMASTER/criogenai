# Research: which model providers to support next (2026-11-04)

Question: which LLM providers do people actually use, which of them Criogen reaches
today, and what it would take to cover the rest. Sources are listed at the end; market
figures are surveys or estimates, not exact counts.

## 1. What people use

| Signal | What it says |
|---|---|
| Menlo Ventures, State of Enterprise AI 2026 (published 2026-11-03, ~400 US companies) | Enterprise API **spend**: Anthropic 55 %, OpenAI 31 %, Google 9 %. Open-weight models serve about 1 in 5 tokens but about 1 in 25 dollars. |
| Stack Overflow Developer Survey 2026 (~11,900 answers on LLMs) | Used at work in the last year: Anthropic 74.6 %, OpenAI 74.3 %, Google 54.5 %, Microsoft 26.4 %, DeepSeek 18.9 %, Mistral 9.8 %. |
| OpenRouter rankings (late Oct 2026, by tokens) | Open-weight models lead developer traffic: DeepSeek V4.1 Flash first, then Z.ai GLM, StepFun, Qwen, Moonshot Kimi; Anthropic and Google next. Trackers disagree on exact shares. |
| Enterprise deployment guides (2026) | Large companies usually reach Claude, GPT and Gemini **through their cloud**: Amazon Bedrock, Google Vertex AI, Microsoft Foundry (Azure). The reasons are data residency, billing and existing contracts. |

Takeaways:

1. **The model vendors are settled:** Anthropic, OpenAI and Google cover most paid use.
2. **The access route matters as much as the vendor.** Enterprises, and hospitals in
   particular (Axon Health), buy through AWS, Azure or Google Cloud, not the vendor's
   own API.
3. **Open-weight models carry a large share of the tokens.** They come mostly from
   DeepSeek, Qwen, GLM and Kimi, are served by their makers or by fast hosts (Groq,
   Cerebras, Together, Fireworks), and are often self-hosted.
4. **OpenAI's new features land in the Responses API only.** OpenAI's migration guide:
   from GPT-5.4 on, Chat Completions does not allow tool calling with reasoning effort
   other than `none`. The error reads "Function tools with reasoning_effort are not
   supported for gpt-5.6-sol in /v1/chat/completions". An agent framework that speaks
   only Chat Completions gets newer OpenAI models without reasoning.

## 2. What Criogen supported on 2026-11-04 (before step 1 below)

Criogen has two wire protocols (`openai_chat`, `anthropic_messages`) and presets on top
of them (`crates/criogen-providers/src/provider.rs`).

| Provider | Today | How |
|---|---|---|
| OpenAI | ✅ `openai:` | Chat Completions only; no Responses API |
| Anthropic | ✅ `anthropic:` / `Anthropic(...)` | Messages API |
| Google Gemini (AI Studio key) | ✅ `gemini:` | Gemini's OpenAI-compatible endpoint, thought signatures |
| OpenRouter | ✅ `openrouter:` | Reaches ~300 models, including every open-weight one above |
| Ollama, LM Studio, vLLM | ✅ | OpenAI-compatible, self-hosted |
| Kaggle Model Proxy | ✅ `kaggle:` | Token minting (design 0020) |
| Any OpenAI-compatible server | ✅ `OpenAICompatible(model, base_url, api_key)` | Works for DeepSeek, Mistral, Groq, xAI, Together, Fireworks, Cerebras, Qwen, Kimi, GLM, Azure v1, but by hand: the user must know the URL and the key variable |
| Any Anthropic-compatible server | ⚠️ Rust only (`Endpoint::anthropic_compatible`) | Not exposed in the Python or TypeScript SDKs |
| Embeddings | ✅ OpenAI, Gemini, Ollama, any OpenAI-compatible | |
| Azure OpenAI / Microsoft Foundry | ⚠️ by hand | The v1 API (`https://<resource>.openai.azure.com/openai/v1/`) is OpenAI-compatible, but Azure keys go in an `api-key` header, and Entra ID tokens need refreshing |
| Amazon Bedrock | ❌ | Needs Bedrock API keys (Bearer) or AWS SigV4 signing; Claude through Bedrock uses the Converse or Invoke APIs |
| Google Vertex AI | ❌ | Needs Google OAuth (service account); Claude on Vertex (`rawPredict`) does not accept API keys |
| OpenAI Responses API | ❌ | Design 0006 Q5 named it the next protocol after the first two |

## 3. The gaps, by impact

| # | Gap | Who needs it | Effort | Why |
|---|---|---|---|---|
| 1 | **OpenAI Responses API** | Everyone using GPT-5.4 and newer with tools | Medium–large: a third protocol (request, stream events, reasoning items, errors), about the size of `anthropic_messages` | Without it, newer OpenAI models run without reasoning when tools are on. This hurts agents most. |
| 2 | **Named presets for popular OpenAI-compatible hosts** | Developers on open-weight and fast models | Small: data only (URL, key variable, quirks), one test each against a local server | `deepseek:`, `mistral:`, `groq:`, `xai:`, `together:`, `fireworks:`, `cerebras:`, `qwen:` (DashScope), `moonshot:` (Kimi), `zai:` (GLM). Today they work only through `OpenAICompatible` with a hand-typed URL. |
| 3 | **Azure OpenAI / Foundry** | Enterprises on Microsoft | Small with an API key (`api-key` header + v1 URL); medium with Entra ID (token refresh, like Kaggle's) | One of the three enterprise routes. Hospitals often have Azure contracts. |
| 4 | **Amazon Bedrock** | Enterprises on AWS, the biggest route for Claude | Medium: Bedrock API keys (Bearer) on the OpenAI-compatible endpoint or the Messages-compatible path; large: SigV4 signing for IAM roles, which needs crypto code or a dependency | One of the three enterprise routes. Must not pull in the AWS SDK (rule 2). |
| 5 | **Google Vertex AI** | Enterprises on Google Cloud | Medium: service-account JWT → OAuth token, refreshed (signing needs RSA); Gemini through the OpenAI-compatible endpoint, Claude through `rawPredict` with the Anthropic protocol | One of the three enterprise routes. |
| 6 | **`AnthropicCompatible` in both SDKs** | DeepSeek, GLM, Qwen and Kimi users who prefer the Anthropic format; any self-hosted Anthropic-style gateway | Tiny: the Rust side exists | Free completeness. |
| 7 | **Model catalog** (context window, prices, capabilities per model) | Cost reports, budgets in money, choosing models | Medium; LiteLLM's MIT data file is an option (needs a `THIRD_PARTY_NOTICES` entry) | The gateway's "budgets in money" is still open. |

Out of scope for now: Cohere, AI21, Perplexity, Hugging Face Inference. Their share is
small, and OpenRouter or `OpenAICompatible` already reach most of them.

## 4. Suggested order

1. **Presets (gap 2) + `AnthropicCompatible` (gap 6):** **done 2026-11-04** (design
   0006 §4.1): DeepSeek, Mistral, xAI, Groq, Cerebras, Together, Fireworks, Qwen, Kimi,
   GLM and LM Studio by name, in both SDKs and the gateway.
2. **OpenAI Responses API (gap 1):** **done 2026-11-10** (design 0006 §16): the
   `openai` preset speaks it; reasoning with tools checked live.
3. **Azure (gap 3):** **done 2026-11-10** (design 0006 §17): API keys, service
   principals and managed identities (no new crypto: client secrets and the metadata
   service). Bedrock with API keys (gap 4): **done 2026-11-11** (design 0006 §18);
   SigV4 for IAM roles still open.
4. **Vertex (gap 5):** **done 2026-11-11** (design 0006 §19) with gcloud credentials, the
   metadata server and tokens; service-account key files (RS256) and SigV4 still open.
   Originally: **Vertex, Entra ID, SigV4:** the credential-heavy routes, when a user such as
   Axon Health needs them; each needs a short design note on keys and tokens (rule 10).

Each step needs a design update first (rule 1: design 0006 §4 presets, a new protocol
section for Responses) and tests against a scripted local server, never the network
(rule 2), plus a manual live check like the existing `tests/live`.

## Sources

- [Menlo Ventures, 2026: The State of Enterprise AI](https://menlovc.com/perspective/2026-the-state-of-enterprise-ai/) and the [press release](https://www.globenewswire.com/news-release/2026/11/03/3394279/0/en/menlo-ventures-2026-state-of-enterprise-ai-report-enterprise-ai-spending-hits-177b-up-5-4x-in-one-year-as-agents-take-over.html)
- [Stack Overflow Developer Survey 2026, LLMs](https://survey.stackoverflow.co/2026/technology/data/llm)
- [OpenRouter rankings](https://openrouter.ai/rankings); trackers: [teamday.ai](https://www.teamday.ai/blog/top-ai-models-openrouter-2026), [tidelines.ai](https://tidelines.ai/openrouter-rankings)
- [OpenAI: Migrate to the Responses API](https://developers.openai.com/api/docs/guides/migrate-to-responses), [OpenAI community: tools with reasoning_effort on Chat Completions](https://community.openai.com/t/gpt-5-6-chat-completion-reasoning-effort-bug-behavior-change/1386454)
- [Azure OpenAI v1 API lifecycle](https://learn.microsoft.com/en-us/azure/foundry/openai/api-version-lifecycle)
- [Amazon Bedrock: Chat Completions API](https://docs.aws.amazon.com/bedrock/latest/userguide/inference-chat-completions-mantle.html), [OpenAI on Amazon Bedrock](https://developers.openai.com/api/docs/guides/amazon-bedrock)
- [Vertex AI express mode API reference](https://cloud.google.com/vertex-ai/generative-ai/docs/start/express-mode/vertex-ai-express-mode-api-reference)
- [Claude on Bedrock, Vertex and Foundry (deployment guide)](https://systemprompt.io/guides/claude-cowork-enterprise-deployment)
- Chinese providers' endpoints (third-party summaries; check each provider's docs): [jadegate guide](https://jadegate-web.pages.dev/guides/chinese-llm-api-authentication-compared)
