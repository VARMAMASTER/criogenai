#!/usr/bin/env bash
# The Python SDK as users get it (design 0011 §5, AGENTS.md rule 9): builds
# the wheel with maturin, installs it into a NEW venv, runs a consumer
# script that uses only the public API, then the test suite against the
# installed wheel (from outside the source tree).
#
# Needs Python 3.9+ (`PYTHON`, default `python`) and Rust; maturin and
# pytest are installed into target/py-build on first use (from PyPI).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python="${PYTHON:-python}"
export CARGO_TARGET_DIR="$root/target/python"
bin_of() { if [[ -d $1/Scripts ]]; then echo "$1/Scripts"; else echo "$1/bin"; fi; }

build="$root/target/py-build"
if [[ ! -x "$(bin_of "$build")/maturin" && ! -x "$(bin_of "$build")/maturin.exe" ]]; then
  "$python" -m venv "$build"
  "$(bin_of "$build")/python" -m pip install --quiet maturin pytest
fi

wheels="$root/target/wheels"
rm -rf "$wheels"
"$(bin_of "$build")/maturin" build --release --quiet \
  --manifest-path "$root/sdks/python/Cargo.toml" --out "$wheels"
wheel="$(ls "$wheels"/agentbuild-*.whl | head -1)"
echo "built $(basename "$wheel")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
"$python" -m venv "$work/venv"
venv_python="$(bin_of "$work/venv")/python"
"$venv_python" -m pip install --quiet "$wheel" pytest

# From a directory without the sources: imports come from the wheel.
cp -r "$root/sdks/python/tests" "$work/tests"
cd "$work"
"$venv_python" "$root/sdks/python/examples/consumer.py"
"$venv_python" -m pytest -q -p no:cacheprovider tests
