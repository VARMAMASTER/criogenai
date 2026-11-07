// Criogen in the browser and other JavaScript hosts (design 0021): the same
// API as on servers, on the same Rust engine compiled to WebAssembly.
//
//   import { Agent, FakeModel, ready } from "criogen/web";
//   await ready();                  // loads the engine, once
//   const agent = new Agent({ model: new FakeModel(["hi"]) });
//   console.log((await agent.run("hello")).text);
//
// Differences, by necessity: storage is in memory (databases need a server),
// telemetry export and web tools run on servers, keys are passed as `apiKey`
// (no environment variables), and model calls go through `fetch` (the
// provider must accept calls from a browser page). A key in a page is visible
// to whoever uses the page: in a product, point models at your own server or
// a Criogen gateway with per-user keys.

import { useEngineLoader } from "../native";
import { webLoader } from "./engine";

useEngineLoader(webLoader);

export { Agent, AgentTool, Decision, Stream, Thread } from "../agent";
export type { AgentOptions, ApprovalRequest, CallOptions, Reply, Retry, RunOptions, RunResult, StreamEvent } from "../agent";
export {
  CriogenError,
  CancelledError,
  LimitExceededError,
  ModelError,
  NativeUnavailableError,
  StorageError,
  ThreadBusyError,
  ThreadSuspendedError,
  ToolExecutionError,
  ValidationError,
  isCriogenError,
} from "../errors";
export {
  Embedder,
  FakeModel,
  FakeReply,
  FunctionModel,
  KaggleModel,
  ProviderModel,
  anthropic,
  anthropicCompatible,
  cerebras,
  deepseek,
  fireworks,
  gemini,
  groq,
  kaggle,
  lmstudio,
  mistral,
  moonshot,
  ollama,
  openai,
  openaiCompatible,
  openrouter,
  qwen,
  together,
  xai,
  zai,
} from "../models";
export type { KaggleOptions, Model, ModelAnswer, ModelCall, ModelOptions } from "../models";
export { ChatStream, Gateway } from "../gateway";
export type { ChatInput, ChatOptions, ChatReply, Chunk, GatewayOptions } from "../gateway";
export { ToolResult } from "../hooks";
export type { Hook, HookAction, HookContext, ToolCall } from "../hooks";
export { Memory, Storage } from "../storage";
export { Collection, CollectionTool } from "../collections";
export type { CollectionOptions, Document, Hit } from "../collections";
export { Telemetry } from "../telemetry";
export type { TelemetryOptions } from "../telemetry";
export type { MemoryOptions, RetentionOptions, StorageOptions } from "../storage";
export { Tool, ToolError, tool } from "../tools";
export type { JsonSchema, StandardIssue, StandardResult, StandardSchema, ToolContext, ToolDefinition } from "../tools";
export { CompiledWorkflow, END, Workflow, WorkflowThread, goto, suspend } from "../workflow";
export type { Goto, Interrupt, NodeContext, NodeFunction, Router, Suspend, WorkflowResult, WorkflowRunOptions } from "../workflow";

export * as patterns from "../patterns";
export * as specialists from "../specialists";
export * as web from "./web";
// MCP servers over HTTP (design 0022); `mcp.stdio` needs a server.
export * as mcp from "../mcp";
export { McpServer, McpTool } from "../mcp";
export type { HttpOptions, McpInfo, McpOptions, StdioOptions } from "../mcp";
export { ready } from "../ready";
export { estimateCost } from "../usage";
export type { Redactor } from "../redact";
export type { ModelUse, Price, RunFacts } from "../usage";
export type { ReadyOptions, ReadyReport } from "../ready";
