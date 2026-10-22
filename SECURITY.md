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

## How AgentBuild is built and released

LiteLLM was compromised twice in 2026 (March and October) through its release
pipeline, not its code. What we do against the same attacks, checked on every
change by `scripts/check-supply-chain.sh`:

- **Every GitHub Action is pinned to a full commit hash**, never a tag or
  branch (a tag can be rewritten, which is how the March attack worked).
- **No install or build runs while a publishing secret exists.** Secrets are
  only in steps that run `publish`; the package is built first, without them.
- **Locked installs.** Python tools install from a hash-locked file
  (`scripts/locks/python-tools.txt`, `pip --require-hashes`); Node packages
  with `npm ci --ignore-scripts`, so a new poisoned release is not picked up
  and install scripts do not run.
- **Python publishes with trusted publishing** (no stored token); npm
  publishes with provenance, moving to trusted publishing after the first
  release ([RELEASING.md](RELEASING.md)).
- **No `pull_request_target`**, and workflows are read-only unless a job
  needs more.
- **The gateway and the SDKs never log prompts, answers or keys**; a test
  fails if a usage record or debug output contains any (October's attacker
  found developers' tokens in logged response bodies).
- Rust dependencies are checked by `cargo deny` (advisories, licences,
  sources) on every change.

## Known limits

- Checkpoints and the SQLite file are **not encrypted at rest** yet
  (planned, design 0008 §6.2). Protect the database file with disk
  encryption and file permissions, and avoid storing secrets in state.
- Model providers see the prompts and tool results you send them. Choose a
  provider (or a self-hosted model) that fits your data rules.
- A tool that crashes mid-call after its side effect, before its result is
  saved, runs again on recovery. Use idempotency keys with external systems
  (design 0008 §5.5).
