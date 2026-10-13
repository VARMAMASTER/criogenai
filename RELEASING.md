# Releasing

All crates share one version and are released together.

## Before the first release (once)

- [ ] Confirm the package names (`agentbuild-core`, `agentbuild-providers`,
      `agentbuild-storage`, `agentbuild-agent`, `agentbuild-telemetry`; free
      on crates.io as of 2026-10-13).
- [ ] Remove `publish = false` (and its comment) from every
      `crates/*/Cargo.toml`.
- [ ] Make the GitHub repository public and turn on **private
      vulnerability reporting** (Settings → Code security), which
      [SECURITY.md](SECURITY.md) points to.
- [ ] Create a crates.io API token (scope: publish-new, publish-update) and
      save it as the repository secret `CARGO_REGISTRY_TOKEN`.

## Every release

1. Make sure `main` is green in CI.
2. Run the live check against real models (`tests/live`, see its README)
   and the demo app by hand; note anything new in the changelog.
3. Set the version in the workspace `Cargo.toml` (`[workspace.package]
   version`) and in the `version = "…"` of every path dependency between
   our crates.
4. In [CHANGELOG.md](CHANGELOG.md), rename **Unreleased** to
   `## [x.y.z] - YYYY-MM-DD` and start a new empty **Unreleased** section.
5. Commit (`chore: release x.y.z`) and push.
6. In GitHub → Actions → **Release** → *Run workflow*: first without
   *publish* (a dry run: tests and packaging), then with *publish*. It
   publishes the crates in dependency order, tags `vx.y.z` and creates a
   GitHub release.
7. Check the crates on crates.io and their docs on docs.rs (all features
   are documented).

A published version cannot be replaced, only yanked. If something is wrong,
fix it and release a new patch version.
