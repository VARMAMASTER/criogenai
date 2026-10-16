// AgentBuild for TypeScript: agents with tools, approvals, durable threads and
// memory, on a Rust engine (design 0012).

export { Agent, Decision, Stream, Thread } from "./agent";
export type { AgentOptions, ApprovalRequest, CallOptions, Reply, RunOptions, RunResult, StreamEvent } from "./agent";
export {
  AgentBuildError,
  CancelledError,
  LimitExceededError,
  ModelError,
  StorageError,
  ThreadBusyError,
  ThreadSuspendedError,
  ToolExecutionError,
  ValidationError,
  isAgentBuildError,
} from "./errors";
export {
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
export { Memory, Storage } from "./storage";
export type { MemoryOptions, RetentionOptions, StorageOptions } from "./storage";
export { Tool, ToolError, tool } from "./tools";
export type { JsonSchema, ToolContext, ToolDefinition } from "./tools";
