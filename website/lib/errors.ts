// The error catalog (../schemas/error-codes.json): the same file the Rust,
// Python and TypeScript tests check against. Read at build time.
import fs from "node:fs";
import path from "node:path";

export interface ErrorEntry {
  code: string;
  layers: string[];
  category: string;
  retryable: boolean | null;
  family: string | null;
  http_status?: number;
  description: string;
}

export const LAYERS: [string, string][] = [
  ["run", "Run errors"],
  ["thread", "Thread errors"],
  ["storage", "Storage errors"],
  ["model", "Model errors (inside model_failed)"],
  ["tool_result", "Tool results (the model sees these; the run goes on)"],
  ["build", "Build errors"],
  ["gateway", "Gateway errors (HTTP)"],
];

export const CLASS: Record<string, string> = {
  model: "ModelError",
  tool: "ToolExecutionError",
  limit: "LimitExceededError",
  busy: "ThreadBusyError",
  suspended: "ThreadSuspendedError",
  storage: "StorageError",
  validation: "ValidationError",
  cancelled: "CancelledError",
  other: "CriogenError",
};

export function errorCatalog(): ErrorEntry[] {
  const file = path.join(process.cwd(), "..", "schemas", "error-codes.json");
  return (JSON.parse(fs.readFileSync(file, "utf8")) as { codes: ErrorEntry[] }).codes;
}
