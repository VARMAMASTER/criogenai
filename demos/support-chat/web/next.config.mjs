// The UI calls `/api/...`; Next forwards it to the Rust server, so the
// browser never talks to it (or to a model provider) directly.
const server = process.env.SUPPORT_SERVER ?? "http://127.0.0.1:8787";

/** @type {import('next').NextConfig} */
const nextConfig = {
  // Do not write AGENTS.md / CLAUDE.md into this folder.
  agentRules: false,
  async rewrites() {
    return [{ source: "/api/:path*", destination: `${server}/api/:path*` }];
  },
  experimental: {
    // Agent turns with tool calls can take a while.
    proxyTimeout: 180_000,
  },
};

export default nextConfig;
