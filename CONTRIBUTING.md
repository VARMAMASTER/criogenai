# Contributing to AgentBuild

Thanks for helping. This page covers how to build, test and propose changes.
[AGENTS.md](AGENTS.md) has the project's rules in full; the essentials are
below.

## Build and test

Rust 1.88 or newer.

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
bash scripts/test-package.sh      # uses the packaged crates from a separate project
bash scripts/get-postgres.sh      # once: portable PostgreSQL in ~/.agentbuild/postgres
bash scripts/with-postgres.sh cargo test --workspace --all-features   # + the Postgres tests
```

The Postgres tests skip unless `AGENTBUILD_TEST_POSTGRES_URL` names a
server. `scripts/with-postgres.sh` starts a throwaway one (no install,
service or Docker) and sets the variable.

`bash scripts/ci-local.sh` runs the whole pipeline on your machine: all of
the above (Postgres included once the binaries are there), a build on
Rust 1.88, every example, and a license and advisory check
(`cargo deny`). For now this local run is the CI: GitHub Actions does
not run on push (the workflow can still be started by hand, and covers
Linux, Windows and macOS).

Optional, needs API keys and costs a few cents: `cd tests/live && cargo run`
checks every feature against real models (see `tests/live/README.md`).

## Rules that keep the project healthy

1. **Contracts before features.** A behaviour change needs an update to a
   design document in `docs/design/` in the same change.
2. **Tests with every behaviour change.** Tests never call real models: use
   `FakeModel` (agents, providers) or the simulated LLM in
   `crates/agentbuild-core/tests/support/` (workflows), on Tokio's paused
   clock where possible. Concurrency claims need a test that would hang or
   fail if the claim were false.
3. **Explicit failures.** Every error has a stable snake_case code.
4. **Strict now, relax later.** New validation starts strict.
5. **Keep dependencies few.** Add one only with a stated reason.
6. **Honest docs.** Planned work is labelled as planned; no performance
   claims without benchmarks in the repository.
7. **Public API hygiene.** Public items have docs and `Debug`; types that may
   grow are `#[non_exhaustive]`.

## Proposing a change

1. For anything beyond a small fix, open an issue first to agree on the
   approach.
2. Use [Conventional Commits](https://www.conventionalcommits.org/)
   (`feat(agent): …`, `fix(storage): …`, `docs: …`); mark breaking changes
   with `!`.
3. Add an entry under **Unreleased** in [CHANGELOG.md](CHANGELOG.md) for
   user-visible changes.
4. Make sure the commands above pass.

## Code you must not bring in

We study other frameworks (LangChain, LangGraph, LiteLLM) but never copy
their code. Code from AGPL projects (for example llmgateway) cannot be used
at all. Data or code taken from MIT-licensed sources needs an entry in a
`THIRD_PARTY_NOTICES` file.

## License

By contributing, you agree that your contributions are dual licensed under
MIT OR Apache-2.0, as the rest of the project.
