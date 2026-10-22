// The AgentBuild website: the landing page, the docs (/docs/) and the
// benchmark lab (/benchmarks/), exported as static files (out/) for Vercel.
import path from "node:path";
import { fileURLToPath } from "node:url";

/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "export",
  trailingSlash: true,
  images: { unoptimized: true },
  // Used at build time only (MDX compile, highlighting); kept out of the bundle.
  serverExternalPackages: ["shiki", "@mdx-js/mdx", "remark-gfm"],
  // The lab's dev fixture is opt-in (scripts/run.mjs --fixture); see lib/bench-data.ts.
  env: { BENCH_FIXTURE: process.env.BENCH_FIXTURE ?? "" },
  // This folder is the app's root (not the home folder with a stray lockfile).
  turbopack: { root: path.dirname(fileURLToPath(import.meta.url)) },
  // `next dev` would otherwise write an AGENTS.md here; the repository has its own.
  agentRules: false,
  // `next dev` seen through an ngrok tunnel (scripts/tunnel.sh): allow its
  // domains to load the dev server's scripts and live reload.
  allowedDevOrigins: ["*.ngrok-free.app", "*.ngrok-free.dev", "*.ngrok.app", "*.ngrok.dev"],
};

export default nextConfig;
