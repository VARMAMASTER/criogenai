"""AgentBuild for Python: agents with tools, approvals, durable threads and
memory, on a Rust engine (design 0011).

    from agentbuild import Agent, tool

    @tool
    def add(a: int, b: int) -> int:
        "Adds two numbers."
        return a + b

    agent = Agent("openai:gpt-5-mini", tools=[add])
    result = agent.run_sync("What is 2 + 3?")
"""

from ._native import __version__
from .agent import Agent, ApprovalRequest, Decision, Event, Reply, RunResult, Stream, Thread
from .errors import (
    AgentBuildError,
    CancelledError,
    LimitExceededError,
    ModelError,
    StorageError,
    ThreadBusyError,
    ThreadSuspendedError,
    ToolExecutionError,
    ValidationError,
)
from .models import (
    Anthropic,
    FakeModel,
    FakeReply,
    Gemini,
    Ollama,
    OpenAI,
    OpenAICompatible,
    OpenRouter,
)
from .storage import Memory, Storage
from .tools import Tool, ToolContext, ToolError, tool

__all__ = [
    "Agent", "AgentBuildError", "Anthropic", "ApprovalRequest", "CancelledError", "Decision",
    "Event", "FakeModel", "FakeReply", "Gemini", "LimitExceededError", "Memory", "ModelError",
    "Ollama", "OpenAI", "OpenAICompatible", "OpenRouter", "Reply", "RunResult", "StorageError",
    "Storage", "Stream", "Thread", "ThreadBusyError", "ThreadSuspendedError", "Tool",
    "ToolContext", "ToolError", "ToolExecutionError", "ValidationError", "__version__", "tool",
]
