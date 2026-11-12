"""CriogenAI for Python: agents with tools, approvals, durable threads and
memory, and workflows, on a Rust engine (designs 0011, 0015).

    from criogenai import Agent, tool

    @tool
    def add(a: int, b: int) -> int:
        "Adds two numbers."
        return a + b

    agent = Agent("openai:gpt-5-mini", tools=[add])
    result = agent.run_sync("What is 2 + 3?")
"""

from ._native import __version__
from .collections import Collection, Document, Hit
from .agent import Agent, AgentTool, ApprovalRequest, Decision, Event, Reply, Retry, RunResult, Stream, Thread
from .errors import (
    CriogenError,
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
    XAI,
    ZAI,
    Anthropic,
    AnthropicCompatible,
    Cerebras,
    DeepSeek,
    Embedder,
    FakeModel,
    FakeReply,
    Fireworks,
    Gemini,
    Groq,
    Kaggle,
    Azure,
    Bedrock,
    Vertex,
    LMStudio,
    Mistral,
    Moonshot,
    Ollama,
    OpenAI,
    OpenAICompatible,
    OpenRouter,
    Qwen,
    Together,
)
from . import env, evals, mcp, patterns, specialists, trace, web
from .env import load_env
from .ready import ready, ready_sync
from .usage import estimate_cost
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
    "END", "Agent", "Collection", "Document", "Hit", "CriogenError", "AgentTool", "Anthropic", "ApprovalRequest", "CancelledError",
    "ChatReply", "ChatStream", "Chunk", "CompiledWorkflow", "Decision", "Embedder", "Event", "FakeModel",
    "FakeReply", "Gateway", "Gemini", "HookAction", "HookContext", "Interrupt", "Kaggle", "Azure", "Bedrock", "Vertex",
    "LimitExceededError", "Memory", "ModelError", "NodeContext", "Ollama", "OpenAI",
    "OpenAICompatible", "OpenRouter", "Reply", "Retry", "RunResult", "StorageError", "Storage", "Stream",
    "Telemetry", "Thread", "ThreadBusyError", "ThreadSuspendedError", "Tool", "ToolCall", "ToolContext", "ToolError",
    "ToolExecutionError", "ToolResult", "ValidationError", "Workflow", "WorkflowResult", "WorkflowThread",
    "AnthropicCompatible", "Cerebras", "DeepSeek", "Fireworks", "Groq", "LMStudio", "Mistral", "Moonshot",
    "Qwen", "Together", "XAI", "ZAI",
    "__version__", "env", "evals", "estimate_cost", "goto", "load_env", "mcp", "ready", "ready_sync", "patterns", "specialists", "suspend", "tool", "trace", "web",
]
