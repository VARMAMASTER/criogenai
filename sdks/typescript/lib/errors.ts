// Errors (design 0012 §3): one class per family, each with the stable `code`
// the Rust engine uses (design 0003).

/** Any AgentBuild failure. `code` is stable across languages. */
export class AgentBuildError extends Error {
  readonly code: string;
  readonly details: Record<string, unknown>;

  constructor(code: string, message: string, details?: Record<string, unknown> | null, cause?: unknown) {
    super(`${code}: ${message}`, cause === undefined ? undefined : { cause });
    this.name = new.target.name;
    this.code = code;
    this.details = details ?? {};
  }
}

/** The model failed after retries (`model_failed`). */
export class ModelError extends AgentBuildError {}
/** A tool crashed (`tool_failed`); `cause` is the thrown value. */
export class ToolExecutionError extends AgentBuildError {}
/** A run limit was reached (`limit_exceeded`). */
export class LimitExceededError extends AgentBuildError {}
/** Another run holds the thread (`thread_busy`). */
export class ThreadBusyError extends AgentBuildError {}
/** The thread waits for approval (`thread_suspended`). */
export class ThreadSuspendedError extends AgentBuildError {}
/** Storage failed (`storage_*`, `encryption_*`, `audit_failed`). */
export class StorageError extends AgentBuildError {}
/** Something given was not accepted (`invalid_*`, `resume_invalid`). */
export class ValidationError extends AgentBuildError {}
/** The run was cancelled (`cancelled`), e.g. by an `AbortSignal`. */
export class CancelledError extends AgentBuildError {}

// By code, as schemas/error-codes.json's `family` says (design 0013); the
// tests check every code against it.
const BY_CODE: Record<string, typeof AgentBuildError> = {
  model_failed: ModelError,
  tool_failed: ToolExecutionError,
  limit_exceeded: LimitExceededError,
  step_limit_exceeded: LimitExceededError,
  deadline_exceeded: LimitExceededError,
  workflow_mismatch: ValidationError,
  thread_busy: ThreadBusyError,
  thread_suspended: ThreadSuspendedError,
  cancelled: CancelledError,
  audit_failed: StorageError,
  resume_invalid: ValidationError,
};

/** The error class for a Rust error code. */
export function errorFor(code: string, message: string, details?: Record<string, unknown> | null, cause?: unknown): AgentBuildError {
  let cls = BY_CODE[code];
  if (!cls) {
    if (/^(storage_|encryption_|checkpoint_)/.test(code)) cls = StorageError;
    else if (code.startsWith("invalid_")) cls = ValidationError;
    else cls = AgentBuildError;
  }
  return new cls(code, message, details, cause);
}

/** `true` if `error` is an AgentBuild error, with `code` if given. */
export function isAgentBuildError(error: unknown, code?: string): error is AgentBuildError {
  return error instanceof AgentBuildError && (code === undefined || error.code === code);
}

/**
 * Turns an error from the addon (its message is `{code, message, details}`)
 * into an `AgentBuildError`. `causes` holds what crashing tools threw, by
 * call id, what failing workflow nodes threw, by `cause_key`, and what
 * failing hooks threw, by `hook:<name>:<stage>`.
 */
export function translate(error: unknown, causes?: Map<string, unknown>): unknown {
  if (!(error instanceof Error)) return error;
  let parsed: { code?: string; message?: string; details?: Record<string, unknown> | null };
  try {
    parsed = JSON.parse(error.message);
  } catch {
    return error;
  }
  if (typeof parsed?.code !== "string") return error;
  let cause: unknown;
  const d = parsed.details;
  const hookKey = typeof d?.["hook"] === "string" ? `hook:${d["hook"]}:${d["stage"]}` : undefined;
  for (const key of [d?.["call_id"], d?.["cause_key"], hookKey]) {
    if (typeof key === "string" && causes?.has(key)) {
      cause = causes.get(key);
      causes.delete(key);
      break;
    }
  }
  return errorFor(parsed.code, parsed.message ?? "", parsed.details, cause);
}
