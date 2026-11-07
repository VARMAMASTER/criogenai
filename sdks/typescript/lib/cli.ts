#!/usr/bin/env node
// `npx criogen trace [--port 4318] [--db file | --memory] [--prices file.json]`:
// the local trace viewer (design 0023).

import { readFileSync } from "node:fs";
import { EvalResult, compare, comparisonReport } from "./evals";
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

// `criogen eval compare base.json new.json [--fail-on-regression]` (design 0024).
async function evalCommand(args: string[]): Promise<number> {
  const files = args.filter((a) => !a.startsWith("--"));
  if (files[0] !== "compare" || files.length !== 3) {
    console.log("usage: criogen eval compare <baseline.json> <current.json> [--fail-on-regression]");
    return 2;
  }
  const [base, current] = await Promise.all([EvalResult.load(files[1]), EvalResult.load(files[2])]);
  const diff = compare(base, current);
  console.log(current.report());
  console.log();
  console.log(comparisonReport(diff));
  const fell = diff.regressed.length > 0 || Object.values(diff.scores).some((d) => d !== null && d < -0.02);
  return args.includes("--fail-on-regression") && fell ? 1 : 0;
}

async function main(): Promise<number> {
  const [command, ...rest] = process.argv.slice(2);
  if (command === "trace") return trace(rest);
  if (command === "eval") return evalCommand(rest);
  console.log("usage: criogen trace [options]                 the local trace viewer (--help for options)");
  console.log("       criogen eval compare <base> <current>   compare two eval results");
  return command ? 2 : 0;
}

main().then(
  (code) => process.exit(code),
  (error) => {
    console.error(`criogen: ${error instanceof Error ? error.message : String(error)}`);
    process.exit(1);
  },
);
