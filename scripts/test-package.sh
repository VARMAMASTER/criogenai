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

# In dependency order: each crate is verified against the unpacked packages
# of the ones before it (they are not on crates.io, so their registry
# dependencies are patched to those packages).
crates=(criogen-core criogen-providers criogen-storage criogen-agent criogen-telemetry criogen-gateway)
template="$root/tests/package/rust-consumer/Cargo.toml.in"
work="$(mktemp -d)"
# The patches below make cargo record `[[patch.unused]]` entries in the
# workspace lockfile; put the original back so they never get committed.
cp "$root/Cargo.lock" "$work/Cargo.lock.orig"
trap 'cp "$work/Cargo.lock.orig" "$root/Cargo.lock"; rm -rf "$work"' EXIT
cp -r "$root/tests/package/rust-consumer/." "$work/"
rm "$work/Cargo.toml.in"
cp "$template" "$work/Cargo.toml"

patches=()
for crate in "${crates[@]}"; do
  version="$(cargo pkgid -p "$crate" | sed 's/.*[@#]//')"
  echo "== packaging $crate $version"
  # --allow-dirty: also test uncommitted changes when run locally.
  cargo package -p "$crate" --allow-dirty "${patches[@]}"

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
  sed "s|$placeholder|$path|g" "$work/Cargo.toml" >"$work/Cargo.toml.new"
  mv "$work/Cargo.toml.new" "$work/Cargo.toml"
  patches+=(--config "patch.crates-io.$crate.path=\"$path\"")
done

if grep -q "_DIR@" "$work/Cargo.toml"; then
  echo "consumer Cargo.toml has an unfilled package path" >&2
  exit 1
fi

echo "== building a consumer project against the packages"
CARGO_TARGET_DIR="$root/target/package-consumer" \
  cargo run --quiet --manifest-path "$work/Cargo.toml"
