// Environment variables where the host has them (Node, Deno, Bun, edge
// runtimes with `process.env`); none in a browser (design 0021), where keys
// are passed as `apiKey` instead.

/** `true` when this host has environment variables (`process.env`). */
export function hasEnv(): boolean {
  return typeof (globalThis as { process?: { env?: unknown } }).process?.env === "object";
}

/** An environment variable, or `undefined` (always, in a browser). */
export function envVar(name: string): string | undefined {
  const env = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env;
  return env?.[name];
}
