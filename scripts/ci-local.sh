#!/usr/bin/env bash
# The CI pipeline, run on this machine. GitHub Actions runs nothing on push
# for now (see .github/workflows/ci.yml); run this before every push.
#
# Steps (the same as the CI jobs, on the local OS only):
#   rust        fmt, clippy (all features and default), tests, doctests,
#               tests without the http feature
#   msrv        cargo check on Rust 1.88 (skipped if that toolchain is not
#               installed: `rustup toolchain install 1.88 --profile minimal`)
#   deps        cargo deny (skipped if cargo-deny is not installed)
#   standalone  fmt + clippy of tests/live and the demo server
#   package     docs, scripts/test-package.sh, every example
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

all=(rust msrv deps standalone package)
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
  run bash scripts/test-package.sh
  run cargo run -p agentbuild-core --example basic_workflow -- "Hello world"
  run cargo run -p agentbuild-core --example approval_workflow -- yes
  run cargo run -p agentbuild-core --example llm_workflow -- "remote work"
  run cargo run -p agentbuild-core --example retry_workflow
  run cargo run -p agentbuild-core --example recover_workflow
  run cargo run -p agentbuild-agent --example approval_agent -- edit
  run cargo run -p agentbuild-agent --example thread_agent
  run cargo run -p agentbuild-agent --features sqlite --example durable_agent
}

for step in "${steps[@]}"; do
  case $step in
    rust | msrv | deps | standalone | package) ;;
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
