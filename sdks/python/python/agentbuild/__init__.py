"""AgentBuild for Python: agents with tools, approvals, durable threads and
memory, and workflows, on a Rust engine (designs 0011, 0015).

    from agentbuild import Agent, tool

    @tool
    def add(a: int, b: int) -> int:
        "Adds two numbers."
        return a + b

    agent = Agent("openai:gpt-5-mini", tools=[add])
    result = agent.run_sync("What is 2 + 3?")
"""

from ._native import __version__
from .agent import Agent, AgentTool, ApprovalRequest, Decision, Event, Reply, Retry, RunResult, Stream, Thread
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
from .workflow import (
    END,
    CompiledWorkflow,
    Interrupt,
    NodeContext,
    Workflow,
    WorkflowResult,
    WorkflowThread,
    goto,
    suspend,
)

__all__ = [
    "END", "Agent", "AgentBuildError", "AgentTool", "Anthropic", "ApprovalRequest", "CancelledError",
    "CompiledWorkflow", "Decision", "Event", "FakeModel", "FakeReply", "Gemini", "Interrupt",
    "LimitExceededError", "Memory", "ModelError", "NodeContext", "Ollama", "OpenAI",
    "OpenAICompatible", "OpenRouter", "Reply", "Retry", "RunResult", "StorageError", "Storage", "Stream",
    "Thread", "ThreadBusyError", "ThreadSuspendedError", "Tool", "ToolContext", "ToolError",
    "ToolExecutionError", "ValidationError", "Workflow", "WorkflowResult", "WorkflowThread",
    "__version__", "goto", "suspend", "tool",
]
