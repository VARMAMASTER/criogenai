# Security policy

## Reporting a vulnerability

Please **do not** open a public issue for a security problem.

Report it privately through GitHub: on the repository page, open the
**Security** tab and choose **Report a vulnerability** (private
vulnerability reporting). Include what you found, how to reproduce it, and
the versions affected. We aim to acknowledge reports within 3 working days
and to agree on a fix and disclosure date with you.

## Supported versions

AgentBuild is in early development (0.x). Security fixes go into the latest
release only.

## What AgentBuild does to protect data

These are design commitments; a bug in any of them is a security issue.

- **API keys** are never logged. They are redacted from model error
  messages (tested), and never stored in checkpoints or snapshots.
- **Run context is not persisted.** Services passed to tools (database
  clients, credentials) are not part of the state and never reach storage.
- **Telemetry content is optional.** The OpenTelemetry exporter can export
  timings and metadata only (`capture_content(false)`), or mask content
  before it leaves the process.
- **Tool arguments are validated** against the tool's JSON Schema before the
  tool runs; invalid calls never reach your code.
- **Human approval** can be required for any tool; nothing in that turn runs
  until a decision is made.
- **Storage is plain JSON**, never a serialized language object: loading a
  checkpoint cannot run code.

## Known limits

- Checkpoints and the SQLite file are **not encrypted at rest** yet
  (planned, design 0008 §6.2). Protect the database file with disk
  encryption and file permissions, and avoid storing secrets in state.
- Model providers see the prompts and tool results you send them. Choose a
  provider (or a self-hosted model) that fits your data rules.
- A tool that crashes mid-call after its side effect, before its result is
  saved, runs again on recovery. Use idempotency keys with external systems
  (design 0008 §5.5).
