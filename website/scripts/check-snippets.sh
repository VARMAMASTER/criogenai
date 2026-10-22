#!/usr/bin/env bash
# Every code sample on the website is a real file in website/snippets,
# checked against the current API: Rust compiled (cargo check), Python
# type-checked with mypy against the SDK's sources and stubs, TypeScript
# type-checked against the SDK's built declarations. Run by
# scripts/ci-local.sh (step `website`).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root"
bin_of() { if [[ -d $1/Scripts ]]; then echo "$1/Scripts"; else echo "$1/bin"; fi; }

echo "+ rust samples"
CARGO_TARGET_DIR="$root/target/website" cargo check --quiet --manifest-path website/snippets/rust/Cargo.toml

echo "+ python samples"
build="$root/target/py-build"
if [[ ! -x "$(bin_of "$build")/python" && ! -x "$(bin_of "$build")/python.exe" ]]; then
  "${PYTHON:-python}" -m venv "$build"
fi
py="$(bin_of "$build")/python"
"$py" -m mypy --version >/dev/null 2>&1 || "$py" -m pip install --quiet --require-hashes -r "$root/scripts/locks/python-tools.txt"
MYPYPATH="$root/sdks/python/python" "$py" -m mypy --ignore-missing-imports --no-error-summary website/snippets/python

echo "+ typescript samples"
if [[ ! -d sdks/typescript/node_modules ]]; then
  (cd sdks/typescript && npm ci --silent --ignore-scripts)
fi
(cd sdks/typescript && npm run --silent build)
sdks/typescript/node_modules/.bin/tsc -p website/snippets/typescript/tsconfig.json

echo "samples ok"
