// [show]
import { Agent, anthropic, anthropicCompatible, gemini, groq, ollama, openai, openaiCompatible, qwen } from "criogenai";

new Agent({ model: "openai:gpt-5-mini" }); // key from OPENAI_API_KEY
new Agent({ model: anthropic("claude-sonnet-5-5") }); // ANTHROPIC_API_KEY
new Agent({ model: gemini("gemini-2.5-flash") }); // GEMINI_API_KEY
new Agent({ model: openai("gpt-5-mini", { apiKey: "sk-…" }) }); // or pass the key yourself

// Open models on fast hosts, by name: deepseek, mistral, xai, groq, cerebras,
// together, fireworks, qwen, moonshot, zai.
new Agent({ model: "deepseek:deepseek-chat" }); // DEEPSEEK_API_KEY
new Agent({ model: groq("llama-3.3-70b-versatile") }); // GROQ_API_KEY
new Agent({ model: qwen("qwen-plus", { baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1" }) }); // China endpoint
new Agent({ model: anthropicCompatible("glm-5.2", { baseUrl: "https://api.z.ai/api/anthropic", apiKey: "…" }) });

// Self-hosted: nothing leaves your network.
new Agent({ model: ollama("llama3.2") });
new Agent({ model: openaiCompatible("qwen3", { baseUrl: "http://gpu-box:8000/v1" }) }); // vLLM, LM Studio, a gateway

// Rate limits, overload and outages go to the next model.
new Agent({ model: "openai:gpt-5-mini", fallbacks: [ollama("llama3.2")], modelRetry: { maxAttempts: 4 } });
// [/show]

// [show direct]
import { Embedder, Gateway } from "criogenai";

const gw = new Gateway({ fallbacks: [ollama("llama3.2")] }); // retries and fallbacks as for an agent
const reply = await gw.chat("openai:gpt-5-mini", "Summarize: …", { instructions: "Two sentences." });
console.log(reply.text, reply.model); // reply.model: the model that answered

for await (const chunk of gw.stream("openai:gpt-5-mini", "Write a haiku")) process.stdout.write(chunk.text);

const triage = { type: "object", properties: { urgent: { type: "boolean" } }, required: ["urgent"] };
const answer = await gw.chat<{ urgent: boolean }>("openai:gpt-5-mini", "Patient reports chest pain.", { output: triage });
answer.output?.urgent; // checked against the schema

const vectors = await gw.embed(Embedder.openai(), ["first note", "second note"]);
// [/show direct]
