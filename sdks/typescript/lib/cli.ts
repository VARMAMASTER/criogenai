#!/usr/bin/env node
// `npx criogen trace [--port 4318] [--db file | --memory] [--prices file.json]`:
// the local trace viewer (design 0023).

import { readFileSync } from "node:fs";
import { serveTraces, type TraceViewerOptions } from "./trace";

const USAGE = `usage: criogen trace [--port N] [--host ADDR] [--db FILE | --memory] [--keep-runs N] [--keep-days N]
                     [--prices FILE.json] [--token-env NAME]`;

async function trace(args: string[]): Promise<number> {
  const options: TraceViewerOptions = {};
  for (let i = 0; i < args.length; i++) {
    const next = () => {
      const value = args[++i];
      if (value === undefined) throw new Error(USAGE);
      return value;
    };
    switch (args[i]) {
      case "--port": options.port = Number(next()); break;
      case "--host": options.host = next(); break;
      case "--db": options.db = next(); break;
      case "--memory": options.db = ":memory:"; break;
      case "--keep-runs": options.keepRuns = Number(next()); break;
      case "--keep-days": options.keepDays = Number(next()); break;
      case "--prices": options.prices = JSON.parse(readFileSync(next(), "utf8")); break;
      case "--token-env": {
        const name = next();
        const token = process.env[name];
        if (!token) throw new Error(`the environment variable ${name} is not set`);
        options.token = token;
        break;
      }
      case "-h": case "--help": console.log(USAGE); return 0;
      default: throw new Error(`unknown argument ${args[i]}\n${USAGE}`);
    }
  }
  const viewer = await serveTraces(options);
  console.log(`Criogen traces: open ${viewer.url}  (send runs with Telemetry.local(); Ctrl+C to stop)`);
  await new Promise<void>((resolve) => {
    process.once("SIGINT", resolve);
    process.once("SIGTERM", resolve);
  });
  await viewer.stop();
  return 0;
}

async function main(): Promise<number> {
  const [command, ...rest] = process.argv.slice(2);
  if (command === "trace") return trace(rest);
  console.log("usage: criogen trace [options]   (the local trace viewer; --help for options)");
  return command ? 2 : 0;
}

main().then(
  (code) => process.exit(code),
  (error) => {
    console.error(`criogen: ${error instanceof Error ? error.message : String(error)}`);
    process.exit(1);
  },
);
