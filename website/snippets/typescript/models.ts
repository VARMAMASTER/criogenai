// [show]
import { Agent, anthropic, gemini, ollama, openai, openaiCompatible } from "@agentbuild/sdk";

new Agent({ model: "openai:gpt-5-mini" }); // key from OPENAI_API_KEY
new Agent({ model: anthropic("claude-sonnet-5-5") }); // ANTHROPIC_API_KEY
new Agent({ model: gemini("gemini-2.5-flash") }); // GEMINI_API_KEY
new Agent({ model: openai("gpt-5-mini", { apiKey: "sk-…" }) }); // or pass the key yourself

// Self-hosted: nothing leaves your network.
new Agent({ model: ollama("llama3.2") });
new Agent({ model: openaiCompatible("qwen3", { baseUrl: "http://gpu-box:8000/v1" }) }); // vLLM, LM Studio, a gateway

// Rate limits, overload and outages go to the next model.
new Agent({ model: "openai:gpt-5-mini", fallbacks: [ollama("llama3.2")], modelRetry: { maxAttempts: 4 } });
// [/show]
