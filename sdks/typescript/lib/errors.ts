// Errors (design 0012 §3): one class per family, each with the stable `code`
// the Rust engine uses (design 0003).

/**
 * Any Criogen failure. `code` is stable across languages.
 *
 * Every error the engine raises is a `CriogenError` or one of its
 * subclasses, chosen by `code`. The message starts with the code
 * (`"limit_exceeded: ..."`). Branch on the class or on `code`, never on the
 * message text.
 *
 * @example
 * import { Agent, FakeModel, FakeReply, LimitExceededError, tool } from "criogen";
 *
 * const ping = tool({ name: "ping", description: "Ping.", input: { type: "object" }, run: () => "pong" });
 * const agent = new Agent({ model: new FakeModel([FakeReply.toolCall("ping")]), tools: [ping], limits: { modelCalls: 1 } });
 * try {
 *   await agent.run("Keep pinging.");
 * } catch (error) {
 *   if (error instanceof LimitExceededError) console.log(error.code, error.details); // limit_exceeded { limit: "model_calls", value: 1 }
 *   else throw error;
 * }
 */
export class CriogenError extends Error {
  /** The stable snake_case error code, e.g. "model_failed" (listed in the errors reference). */
  readonly code: string;
  /** Structured facts about the failure (which limit, which call, ...); `{}` when there are none. */
  readonly details: Record<string, unknown>;

  /**
   * Errors are made by the SDK; you rarely construct one yourself.
   * @param code The stable error code.
   * @param message What went wrong; the error's `message` is `"<code>: <message>"`.
   * @param details Structured facts about the failure.
   * @param cause What caused it, e.g. the value a crashing tool threw; becomes the error's `cause`.
   */
  constructor(code: string, message: string, details?: Record<string, unknown> | null, cause?: unknown) {
    super(`${code}: ${message}`, cause === undefined ? undefined : { cause });
    this.name = new.target.name;
    this.code = code;
    this.details = details ?? {};
  }
}

/**
 * The native engine cannot be loaded on this system (`native_unavailable`).
 * Thrown on first use, never at import: `details` names the platform, the
 * architecture, the C library and the package that was tried.
 */
export class NativeUnavailableError extends CriogenError {}

/** The model failed after retries (`model_failed`). */
export class ModelError extends CriogenError {}
/** A tool crashed (`tool_failed`); `cause` is the thrown value. */
export class ToolExecutionError extends CriogenError {}
/** A run limit was reached (`limit_exceeded`). */
export class LimitExceededError extends CriogenError {}
/** Another run holds the thread (`thread_busy`). */
export class ThreadBusyError extends CriogenError {}
/** The thread waits for approval (`thread_suspended`). */
export class ThreadSuspendedError extends CriogenError {}
/** Storage failed (`storage_*`, `encryption_*`, `audit_failed`). */
export class StorageError extends CriogenError {}
/** Something given was not accepted (`invalid_*`, `resume_invalid`). */
export class ValidationError extends CriogenError {}
/** The run was cancelled (`cancelled`), e.g. by an `AbortSignal`. */
export class CancelledError extends CriogenError {}

// By code, as schemas/error-codes.json's `family` says (design 0013); the
// tests check every code against it.
const BY_CODE: Record<string, typeof CriogenError> = {
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
  native_unavailable: NativeUnavailableError,
};

/** The error class for a Rust error code. */
export function errorFor(code: string, message: string, details?: Record<string, unknown> | null, cause?: unknown): CriogenError {
  let cls = BY_CODE[code];
  if (!cls) {
    if (/^(storage_|encryption_|checkpoint_)/.test(code)) cls = StorageError;
    else if (code.startsWith("invalid_")) cls = ValidationError;
    else cls = CriogenError;
  }
  return new cls(code, message, details, cause);
}

/**
 * `true` if `error` is a Criogen error, with `code` if given.
 *
 * @param error Any caught value.
 * @param code Also require this error code, e.g. "thread_busy".
 * @example
 * import { isCriogenError } from "criogen";
 *
 * try {
 *   await thread.send("Hello");
 * } catch (error) {
 *   if (isCriogenError(error, "thread_busy")) console.log("Try again in a moment.");
 *   else throw error;
 * }
 */
export function isCriogenError(error: unknown, code?: string): error is CriogenError {
  return error instanceof CriogenError && (code === undefined || error.code === code);
}

/**
 * Turns an error from the addon (its message is `{code, message, details}`)
 * into an `CriogenError`. `causes` holds what crashing tools threw, by
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
  for (const key of [d?.["call_id"], d?.["cause_key"]]) {
    if (typeof key === "string" && causes?.has(key)) {
      cause = causes.get(key);
      causes.delete(key);
      break;
    }
  }
  return errorFor(parsed.code, parsed.message ?? "", parsed.details, cause);
}
