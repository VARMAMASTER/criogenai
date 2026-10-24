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
    Embedder,
    FakeModel,
    FakeReply,
    Gemini,
    Ollama,
    OpenAI,
    OpenAICompatible,
    OpenRouter,
)
from . import patterns, specialists, web
from .gateway import ChatReply, ChatStream, Chunk, Gateway
from .hooks import HookAction, HookContext, ToolCall, ToolResult
from .storage import Memory, Storage
from .telemetry import Telemetry
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
    "ChatReply", "ChatStream", "Chunk", "CompiledWorkflow", "Decision", "Embedder", "Event", "FakeModel",
    "FakeReply", "Gateway", "Gemini", "HookAction", "HookContext", "Interrupt",
    "LimitExceededError", "Memory", "ModelError", "NodeContext", "Ollama", "OpenAI",
    "OpenAICompatible", "OpenRouter", "Reply", "Retry", "RunResult", "StorageError", "Storage", "Stream",
    "Telemetry", "Thread", "ThreadBusyError", "ThreadSuspendedError", "Tool", "ToolCall", "ToolContext", "ToolError",
    "ToolExecutionError", "ToolResult", "ValidationError", "Workflow", "WorkflowResult", "WorkflowThread",
    "__version__", "goto", "patterns", "specialists", "suspend", "tool", "web",
]
