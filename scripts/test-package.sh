#!/usr/bin/env bash
# Tests the crates as published packages would be used:
#
# 1. `cargo package` builds each .crate and compiles it in isolation, which
#    fails if a file the crate needs is missing from the package.
# 2. Each package must contain its README and both license files.
# 3. A separate project (tests/package/rust-consumer) depends on the unpacked
#    packages, outside this workspace, and exercises the public API.
#
# Usage: scripts/test-package.sh   (from anywhere; needs cargo on PATH)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

crates=(agentbuild-core agentbuild-providers)
template="$root/tests/package/rust-consumer/Cargo.toml.in"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp -r "$root/tests/package/rust-consumer/." "$work/"
rm "$work/Cargo.toml.in"
cp "$template" "$work/Cargo.toml"

for crate in "${crates[@]}"; do
  version="$(cargo pkgid -p "$crate" | sed 's/.*[@#]//')"
  echo "== packaging $crate $version"
  # --allow-dirty: also test uncommitted changes when run locally.
  cargo package -p "$crate" --allow-dirty

  package_dir="$root/target/package/$crate-$version"
  for file in README.md LICENSE-MIT LICENSE-APACHE Cargo.toml src/lib.rs; do
    if [[ ! -f "$package_dir/$file" ]]; then
      echo "$crate package is missing $file" >&2
      exit 1
    fi
  done
  echo "== $crate package contents ok"

  # Cargo accepts forward slashes on every platform.
  path="$package_dir"
  if command -v cygpath >/dev/null 2>&1; then
    path="$(cygpath -m "$package_dir")"
  fi
  placeholder="@$(echo "$crate" | tr '[:lower:]-' '[:upper:]_')_DIR@"
  # Not `sed -i`: its syntax differs between GNU and BSD (macOS) sed.
  sed "s|$placeholder|$path|" "$work/Cargo.toml" >"$work/Cargo.toml.new"
  mv "$work/Cargo.toml.new" "$work/Cargo.toml"
done

if grep -q "_DIR@" "$work/Cargo.toml"; then
  echo "consumer Cargo.toml has an unfilled package path" >&2
  exit 1
fi

echo "== building a consumer project against the packages"
CARGO_TARGET_DIR="$root/target/package-consumer" \
  cargo run --quiet --manifest-path "$work/Cargo.toml"
