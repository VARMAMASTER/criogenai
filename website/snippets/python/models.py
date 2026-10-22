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
