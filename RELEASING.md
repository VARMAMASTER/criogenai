# Releasing

All crates and both SDKs share one version and are released together.

## Before the first release (once)

- [ ] Confirm the package names. Free as of 2026-10-16: on crates.io
      `agentbuild-core`, `agentbuild-providers`, `agentbuild-storage`,
      `agentbuild-agent`, `agentbuild-telemetry`, `agentbuild-gateway`;
      on PyPI `agentbuild`; on npm `@agentbuild/sdk` (the unscoped
      `agentbuild` is taken). `agentbuild-bench` is never published.
- [ ] Remove `publish = false` (and its comment) from every published
      `crates/*/Cargo.toml` (not `agentbuild-bench`).
- [ ] Make the GitHub repository public and turn on **private
      vulnerability reporting** (Settings → Code security), which
      [SECURITY.md](SECURITY.md) points to.
- [ ] Create a crates.io API token (scope: publish-new, publish-update) and
      save it as the repository secret `CARGO_REGISTRY_TOKEN`.
- [ ] PyPI: create the `agentbuild` project's **trusted publisher** for
      this repository, workflow `sdk-packages.yml`, environment `release`
      (no token is stored).
- [ ] npm: create the `agentbuild` organisation (for the `@agentbuild`
      scope), an automation token, and save it as the repository secret
      `NPM_TOKEN`. Create the GitHub environment `release`.

## Every release

1. Make sure `main` passes `bash scripts/ci-local.sh`. Running the CI
   workflow by hand (GitHub → Actions → **CI** → *Run workflow*) adds Linux
   and macOS; it uses paid Actions minutes while the repository is private
   (free once it is public; macOS minutes cost the most).
2. Run the live checks against real models by hand (`tests/live`,
   `sdks/python/examples/live.py`, `sdks/typescript/examples/live.mjs`),
   and the demo app; note anything new in the changelog.
3. Set the version in the workspace `Cargo.toml` (`[workspace.package]
   version`), in the `version = "…"` of every path dependency between
   our crates, in `sdks/python/pyproject.toml` and `sdks/python/Cargo.toml`,
   and in `sdks/typescript/package.json` and `sdks/typescript/Cargo.toml`.
4. In [CHANGELOG.md](CHANGELOG.md), rename **Unreleased** to
   `## [x.y.z] - YYYY-MM-DD` and start a new empty **Unreleased** section.
5. Commit (`chore: release x.y.z`) and push.
6. In GitHub → Actions → **Release** → *Run workflow*: first without
   *publish* (a dry run: tests and packaging), then with *publish*. It
   publishes the crates in dependency order, tags `vx.y.z` and creates a
   GitHub release.
7. GitHub → Actions → **SDK packages** → *Run workflow*: first without
   *publish*. It builds the Python wheel and the npm native addon on
   Linux (x64, arm64), macOS (arm64, x64) and Windows (x64), and tests
   each package installed in a fresh environment on its own platform;
   the packages are kept as artifacts. Then with *publish*: wheels to
   PyPI, the platform packages `@agentbuild/sdk-<platform>-<arch>` to
   npm, then `@agentbuild/sdk` with them as optional dependencies.
8. Check the crates on crates.io and docs.rs, `pip install agentbuild-sdk`
   and `npm install @agentbuild/sdk` on a clean machine.

A published version cannot be replaced, only yanked. If something is wrong,
fix it and release a new patch version.
