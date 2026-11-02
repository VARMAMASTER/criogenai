# Examples

Small, copyable server setups for the `criogen` npm package. The engine is native
code: it runs on your server (Node.js 18+, the Node runtime, never Edge or the
browser). See the guide "Using Criogen in your app" on the website.

| Folder | What it shows |
|---|---|
| `nextjs-route/` | A Next.js route handler (also a Vercel function): an agent with a tool, limits, redaction, a fallback, timeouts and clear errors; a `/api/health` route with `ready()`; `next.config.mjs` keeping the engine out of the bundle |
| `nest-service/` | A NestJS service and controller: the agent built once on first use, `.env` keys with `loadEnv()`, a health endpoint |

Keys go in server environment variables (`KAGGLE_API_TOKEN`, `OPENAI_API_KEY`, ...),
never in client code or `NEXT_PUBLIC_*` variables. Kaggle's model allowance is for
prototypes and tests: never send patient or customer data through it.
