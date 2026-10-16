#!/usr/bin/env bash
# The CI pipeline, run on this machine. GitHub Actions runs nothing on push
# for now (see .github/workflows/ci.yml); run this before every push.
#
# Steps (the same as the CI jobs, on the local OS only):
#   rust        fmt, clippy (all features and default), tests, doctests,
#               tests without the http feature
#   postgres    the Postgres tests against a throwaway server
#               (scripts/with-postgres.sh; skipped without binaries:
#               `bash scripts/get-postgres.sh` fetches portable ones)
#   msrv        cargo check on Rust 1.88 (skipped if that toolchain is not
#               installed: `rustup toolchain install 1.88 --profile minimal`)
#   deps        cargo deny (skipped if cargo-deny is not installed)
#   standalone  fmt + clippy of tests/live and the demo server
#   package     docs, scripts/test-package.sh (with a throwaway Postgres
#               when available), every example
#   python      the Python SDK: lint of its bridge, scripts/test-python.sh
#               (wheel into a new venv; skipped without Python 3.9+)
#   typescript  the TypeScript SDK: lint of its bridge, type check,
#               scripts/test-typescript.sh (tarball into a new project;
#               skipped without Node 18+)
#
# Usage: scripts/ci-local.sh            all steps
#        scripts/ci-local.sh --quick    rust step only
#        scripts/ci-local.sh msrv deps  only the named steps
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export CARGO_TERM_COLOR=always
# Incremental build files take a lot of disk and do not help a full run.
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

all=(rust postgres msrv deps standalone package python typescript)
if [[ $# -eq 0 ]]; then
  steps=("${all[@]}")
elif [[ $1 == --quick ]]; then
  steps=(rust)
else
  steps=("$@")
fi

started=$SECONDS
passed=()
skipped=()

# Runs one command; the first failure ends the pipeline.
run() {
  echo "+ $*"
  if ! "$@"; then
    echo
    echo "FAILED in step $current: $*" >&2
    echo "time: $((SECONDS - started))s" >&2
    exit 1
  fi
}

step_rust() {
  run cargo fmt --all --check
  run cargo clippy --all-targets --all-features -- -D warnings
  run cargo clippy --all-targets -- -D warnings
  run cargo test --all-targets --all-features
  run cargo test --doc --workspace --all-features
  run cargo test -p agentbuild-providers --no-default-features
  run cargo test -p agentbuild-agent --no-default-features
}

# A server for the Postgres tests: given, or startable by with-postgres.sh.
have_postgres() {
  [[ -n ${AGENTBUILD_TEST_POSTGRES_URL:-} || -n ${PG_BIN:-} ]] ||
    command -v pg_ctl >/dev/null 2>&1 ||
    [[ -d ${PG_HOME:-$HOME/.agentbuild/postgres}/bin ]]
}

step_postgres() {
  if ! have_postgres; then
    skipped+=("postgres (no binaries: bash scripts/get-postgres.sh)")
    return
  fi
  # REQUIRE: the tests fail instead of skipping if the server is missing.
  AGENTBUILD_REQUIRE_POSTGRES=1 run bash scripts/with-postgres.sh \
    cargo test -p agentbuild-storage -p agentbuild-agent --all-features \
    --test postgres --test memory_store --test durable --test encryption --test retention --test audit
}

step_msrv() {
  if ! rustup run 1.88 rustc --version >/dev/null 2>&1; then
    skipped+=("msrv (no 1.88 toolchain)")
    return
  fi
  run cargo +1.88 check --all-targets --all-features
}

step_deps() {
  if ! cargo deny --version >/dev/null 2>&1; then
    skipped+=("deps (cargo-deny not installed)")
    return
  fi
  run cargo deny --all-features check
}

step_standalone() {
  # Separate target dirs, kept between runs (they are not workspace members).
  CARGO_TARGET_DIR="$root/target/live" run cargo fmt --manifest-path tests/live/Cargo.toml --check
  CARGO_TARGET_DIR="$root/target/live" run cargo clippy --manifest-path tests/live/Cargo.toml -- -D warnings
  CARGO_TARGET_DIR="$root/target/demo" run cargo fmt --manifest-path demos/support-chat/server/Cargo.toml --check
  CARGO_TARGET_DIR="$root/target/demo" run cargo clippy --manifest-path demos/support-chat/server/Cargo.toml -- -D warnings
}

step_package() {
  RUSTDOCFLAGS="-D warnings" run cargo doc --no-deps --all-features
  if have_postgres; then
    run bash scripts/with-postgres.sh bash scripts/test-package.sh
  else
    run bash scripts/test-package.sh
  fi
  # The benchmarks keep working (debug build; numbers not recorded).
  run cargo run -p agentbuild-bench -- quick
  run cargo run -p agentbuild-core --example basic_workflow -- "Hello world"
  run cargo run -p agentbuild-core --example approval_workflow -- yes
  run cargo run -p agentbuild-core --example llm_workflow -- "remote work"
  run cargo run -p agentbuild-core --example retry_workflow
  run cargo run -p agentbuild-core --example recover_workflow
  run cargo run -p agentbuild-agent --example approval_agent -- edit
  run cargo run -p agentbuild-agent --example thread_agent
  run cargo run -p agentbuild-agent --features sqlite --example durable_agent
}

step_python() {
  local python="${PYTHON:-python}"
  if ! "$python" -c 'import sys; sys.exit(sys.version_info < (3, 9))' >/dev/null 2>&1; then
    skipped+=("python (no Python 3.9+: set PYTHON)")
    return
  fi
  local manifest=sdks/python/Cargo.toml
  CARGO_TARGET_DIR="$root/target/python" run cargo fmt --manifest-path "$manifest" --check
  PYO3_PYTHON="$python" CARGO_TARGET_DIR="$root/target/python"     run cargo clippy --manifest-path "$manifest" -- -D warnings
  run bash scripts/test-python.sh
}

step_typescript() {
  if ! node -e 'process.exit(Number(process.versions.node.split(".")[0]) < 18 ? 1 : 0)' >/dev/null 2>&1; then
    skipped+=("typescript (no Node 18+)")
    return
  fi
  local manifest=sdks/typescript/Cargo.toml
  CARGO_TARGET_DIR="$root/target/node" run cargo fmt --manifest-path "$manifest" --check
  CARGO_TARGET_DIR="$root/target/node" run cargo clippy --manifest-path "$manifest" -- -D warnings
  run bash scripts/test-typescript.sh
}

for step in "${steps[@]}"; do
  case $step in
    rust | postgres | msrv | deps | standalone | package | python | typescript) ;;
    *)
      echo "unknown step: $step (steps: ${all[*]})" >&2
      exit 2
      ;;
  esac
  echo
  echo "=== $step ==="
  current=$step
  before=${#skipped[@]}
  "step_$step"
  [[ ${#skipped[@]} -eq $before ]] && passed+=("$step")
done

echo
echo "passed:  ${passed[*]:-none}"
if [[ ${#skipped[@]} -gt 0 ]]; then
  echo "skipped: ${skipped[*]}"
fi
echo "time:    $((SECONDS - started))s"
