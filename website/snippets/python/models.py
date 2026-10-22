# [show]
from agentbuild import Agent, Anthropic, Gemini, Ollama, OpenAI, OpenAICompatible, Retry

Agent("openai:gpt-5-mini")  # key from OPENAI_API_KEY
Agent(Anthropic("claude-sonnet-5-5"))  # ANTHROPIC_API_KEY
Agent(Gemini("gemini-2.5-flash"))  # GEMINI_API_KEY
Agent(OpenAI("gpt-5-mini", api_key="sk-…"))  # or pass the key yourself

# Self-hosted: nothing leaves your network.
Agent(Ollama("llama3.2"))
Agent(OpenAICompatible("qwen3", base_url="http://gpu-box:8000/v1"))  # vLLM, LM Studio, a gateway

# Rate limits, overload and outages go to the next model.
Agent("openai:gpt-5-mini", fallbacks=[Ollama("llama3.2")], model_retry=Retry(max_attempts=4))
# [/show]


async def direct() -> None:
    # [show direct]
    from agentbuild import Embedder, Gateway, Ollama

    gw = Gateway(fallbacks=[Ollama("llama3.2")])  # retries and fallbacks as for an agent
    reply = await gw.chat("openai:gpt-5-mini", "Summarize: …", instructions="Two sentences.")
    print(reply.text, reply.model)  # reply.model: the model that answered

    async for chunk in gw.stream("openai:gpt-5-mini", "Write a haiku"):
        print(chunk.text, end="")

    triage = {"type": "object", "properties": {"urgent": {"type": "boolean"}}, "required": ["urgent"]}
    reply = await gw.chat("openai:gpt-5-mini", "Patient reports chest pain.", output=triage)
    reply.output  # {"urgent": True}, checked against the schema

    vectors = await gw.embed(Embedder.openai(), ["first note", "second note"])
    # [/show direct]
