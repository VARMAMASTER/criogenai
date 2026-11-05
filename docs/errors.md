# Error codes

Generated from [`schemas/error-codes.json`](../schemas/error-codes.json) (design 0013); do not edit by hand. Codes are stable: they may be added, never renamed or removed before 1.0.

*SDK class* is the Python / TypeScript error class a run error with that code raises (`-` for codes that appear inside other errors or messages).

## Run errors

| Code | Category | Retryable | SDK class | HTTP | Meaning |
|---|---|---|---|---|---|
| `node_failed` | developer | no | `CriogenError` | - | A workflow node returned an error. |
| `node_panicked` | developer | no | `CriogenError` | - | A Rust node panicked; the run failed instead of the process. |
| `node_timeout` | developer | yes | `CriogenError` | - | A node attempt took longer than its policy timeout. |
| `router_failed` | developer | no | `CriogenError` | - | A router function returned an error. |
| `unknown_route` | developer | no | `CriogenError` | - | A router returned a label with no edge. |
| `missing_goto` | developer | no | `CriogenError` | - | A node that must choose the next node did not. |
| `memo_type_mismatch` | developer | no | `CriogenError` | - | A saved task memo entry does not have the shape the node asks for (or a value to save is not JSON); seen as the kind of a node_failed error. |
| `update_conflict` | developer | no | `CriogenError` | - | Two nodes wrote the same state key in one step without a reducer. |
| `step_limit_exceeded` | policy | no | `LimitExceededError` | - | The run reached its step limit. |
| `deadline_exceeded` | policy | no | `LimitExceededError` | - | The run reached its deadline, or the next retry would pass it. |
| `cancelled` | cancellation | yes | `CancelledError` | - | The run was cancelled (a cancel token, an AbortSignal, a dropped stream, an interrupt from another process). In a tool message: a call that never got a result. |
| `invalid_value` | developer | no | `ValidationError` | - | A value was not accepted: a state write a reducer cannot apply, a memory key, value, namespace or vector, a configuration value. |
| `resume_invalid` | developer | no | `ValidationError` | - | A resume or approval decision does not match what the run waits for. |
| `workflow_mismatch` | developer | no | `ValidationError` | - | A snapshot was made by a different workflow. |
| `storage_failed` | transient | yes | `StorageError` | - | Saving the run's progress failed; details carry the storage code. |
| `model_failed` | provider | depends | `ModelError` | - | A model call failed after retries and fallbacks; details.model_error has the kind, and retryable follows it. |
| `tool_failed` | developer | no | `ToolExecutionError` | - | A tool failed with an error it did not handle (an exception, a panic). By default the model sees it as the call's result and the run goes on; with on_tool_exception="fail" (or ToolError::fatal) it fails the run. |
| `limit_exceeded` | policy | no | `LimitExceededError` | - | A model-call, tool-call, token or nesting limit was reached; details.limit names it. |
| `output_invalid` | policy | no | `CriogenError` | - | The structured output was still invalid after the allowed retries. |
| `hook_failed` | developer | no | `CriogenError` | - | A hook returned an error. |
| `invalid_state` | developer | no | `ValidationError` | - | The run state is not an agent's, or memory cannot find the run's namespace. |
| `invalid_input` | developer | no | `ValidationError` | - | The input names one of the agent's own state keys, or tries to change the value that scopes the thread's memory. |
| `audit_failed` | transient | yes | `StorageError` | - | An audit record could not be written, so the action it describes failed. |

## Thread errors

| Code | Category | Retryable | SDK class | HTTP | Meaning |
|---|---|---|---|---|---|
| `thread_busy` | transient | yes | `ThreadBusyError` | - | Another run holds the thread (OnBusy reject, or the wait timed out). |
| `thread_suspended` | developer | no | `ThreadSuspendedError` | - | The thread waits for approval: answer it before sending more. |
| `thread_interrupted` | developer | no | `CriogenError` | - | The thread's last run did not finish: continue it with retry. |

## Storage errors

| Code | Category | Retryable | SDK class | HTTP | Meaning |
|---|---|---|---|---|---|
| `invalid_value` | developer | no | `ValidationError` | - | A value was not accepted: a state write a reducer cannot apply, a memory key, value, namespace or vector, a configuration value. |
| `audit_failed` | transient | yes | `StorageError` | - | An audit record could not be written, so the action it describes failed. |
| `thread_busy` | transient | yes | `ThreadBusyError` | - | Another run holds the thread (OnBusy reject, or the wait timed out). |
| `storage_unavailable` | transient | yes | `StorageError` | - | The database could not be reached or timed out. |
| `storage_conflict` | transient | yes | `StorageError` | - | Another writer changed the thread first. |
| `checkpoint_not_found` | developer | no | `StorageError` | - | No checkpoint with that id. |
| `storage_corrupt` | internal | no | `StorageError` | - | Stored data could not be read. |
| `storage_incompatible` | developer | no | `StorageError` | - | The database was written by a newer schema version. |
| `encryption_key_missing` | developer | no | `StorageError` | - | Encrypted data, or an encrypted database, without the key to read it. |
| `encryption_key_invalid` | developer | no | `StorageError` | - | A key does not match the one the database knows under its id. |

## Model errors (`details.model_error.kind` of `model_failed`)

| Code | Category | Retryable | SDK class | HTTP | Meaning |
|---|---|---|---|---|---|
| `rate_limited` | transient | yes | - | 429 | The provider asked us to slow down. |
| `overloaded` | transient | yes | - | 503 | The provider is overloaded. |
| `server` | transient | yes | - | 502 | The provider failed (5xx). |
| `timeout` | transient | yes | - | 504 | No answer in time: a model call, or (in a tool message) a tool call. |
| `network` | transient | yes | - | 502 | The connection failed or broke. |
| `auth` | provider_fatal | no | - | 502 | The provider refused the API key. |
| `permission` | provider_fatal | no | - | 502 | The key may not use this model or feature. |
| `not_found` | provider_fatal | no | - | 502 | The provider does not know the model or endpoint. |
| `invalid_request` | provider_fatal | no | - | 400 | The provider (or the gateway) rejected the request as malformed. |
| `context_overflow` | provider_fatal | no | - | 400 | The conversation is longer than the model's context. |
| `content_filter` | provider_fatal | no | - | 400 | The provider's safety system refused the request. |
| `quota_exhausted` | provider_fatal | no | - | 502 | Billing or quota is exhausted; another provider may still answer. |
| `invalid_response` | provider_fatal | no | - | 502 | A successful response whose body could not be read (already billed, so not retried). |
| `unknown` | provider_fatal | no | - | 502 | An unclassified provider failure. |

## Tool results (`error.code` of a tool message; the run goes on)

| Code | Category | Retryable | SDK class | HTTP | Meaning |
|---|---|---|---|---|---|
| `cancelled` | cancellation | yes | `CancelledError` | - | The run was cancelled (a cancel token, an AbortSignal, a dropped stream, an interrupt from another process). In a tool message: a call that never got a result. |
| `tool_failed` | developer | no | `ToolExecutionError` | - | A tool failed with an error it did not handle (an exception, a panic). By default the model sees it as the call's result and the run goes on; with on_tool_exception="fail" (or ToolError::fatal) it fails the run. |
| `timeout` | transient | yes | - | 504 | No answer in time: a model call, or (in a tool message) a tool call. |
| `invalid_arguments` | model_recoverable | no | - | - | The model's arguments did not match the tool's schema; the model sees the problems and may correct them. |
| `unknown_tool` | model_recoverable | no | - | - | The model called a tool the agent does not have. |
| `tool_error` | model_recoverable | no | - | - | The tool reported a failure meant for the model (ToolError). |

## Build errors

| Code | Category | Retryable | SDK class | HTTP | Meaning |
|---|---|---|---|---|---|
| `invalid_agent` | developer | no | `ValidationError` | - | An SDK agent configuration the engine could not build. |
| `invalid_workflow` | developer | no | `ValidationError` | - | An SDK workflow that does not validate (`details.issues` lists the problems). |
| `unsupported` | developer | no | `CriogenError` | - | The feature needs a server, and here the engine runs as WebAssembly in a JavaScript host (the browser, design 0021): databases (SQLite, Postgres), encryption at rest, telemetry export, web tools. |

## Gateway errors (`error.code` in the HTTP body)

| Code | Category | Retryable | SDK class | HTTP | Meaning |
|---|---|---|---|---|---|
| `invalid_request` | provider_fatal | no | - | 400 | The provider (or the gateway) rejected the request as malformed. |
| `invalid_api_key` | developer | no | - | 401 | The gateway needs a valid virtual key. |
| `model_not_found` | developer | no | - | 404 | No route by that name, or the key may not use it. |
| `key_rate_limited` | transient | yes | - | 429 | The key's requests or tokens per minute are used up; Retry-After says when to try again. |
| `concurrency_limited` | transient | yes | - | 429 | Too many requests are in flight, over the gateway or for this key; Retry-After says when to retry. |
| `budget_exceeded` | policy | no | - | 429 | The key's token budget is spent for the current day or month (Retry-After: until it ends), or in total. |
