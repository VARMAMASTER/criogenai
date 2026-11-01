// Criogen for TypeScript: agents with tools, approvals, durable threads and
// memory, on a Rust engine (design 0012).

export { Agent, AgentTool, Decision, Stream, Thread } from "./agent";
export type { AgentOptions, ApprovalRequest, CallOptions, Reply, Retry, RunOptions, RunResult, StreamEvent } from "./agent";
export {
  CriogenError,
  CancelledError,
  LimitExceededError,
  ModelError,
  StorageError,
  ThreadBusyError,
  ThreadSuspendedError,
  ToolExecutionError,
  ValidationError,
  isCriogenError,
} from "./errors";
export {
  Embedder,
  FakeModel,
  FakeReply,
  ProviderModel,
  anthropic,
  gemini,
  ollama,
  openai,
  openaiCompatible,
  openrouter,
} from "./models";
export type { Model, ModelOptions } from "./models";
export { ChatStream, Gateway } from "./gateway";
export type { ChatInput, ChatOptions, ChatReply, Chunk, GatewayOptions } from "./gateway";
export { ToolResult } from "./hooks";
export type { Hook, HookAction, HookContext, ToolCall } from "./hooks";
export { Memory, Storage } from "./storage";
export { Telemetry } from "./telemetry";
export type { TelemetryOptions } from "./telemetry";
export type { MemoryOptions, RetentionOptions, StorageOptions } from "./storage";
export { Tool, ToolError, tool } from "./tools";
export type { JsonSchema, StandardIssue, StandardResult, StandardSchema, ToolContext, ToolDefinition } from "./tools";
export { CompiledWorkflow, END, Workflow, WorkflowThread, goto, suspend } from "./workflow";
export type { Goto, Interrupt, NodeContext, NodeFunction, Router, Suspend, WorkflowResult, WorkflowRunOptions } from "./workflow";

// Patterns, specialists and web tools (design 0019).
export * as patterns from "./patterns";
export * as specialists from "./specialists";
export * as web from "./web";
export { loadEnv, parseEnv, envStatus, keyName } from "./env";
export type { LoadEnvOptions } from "./env";
