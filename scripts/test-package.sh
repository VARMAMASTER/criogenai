#!/usr/bin/env bash
# Tests agentbuild-core as a published package would be used:
#
# 1. `cargo package` builds the .crate and compiles it in isolation, which
#    fails if a file the crate needs is missing from the package.
# 2. The package must contain the README and both license files.
# 3. A separate project (tests/package/rust-consumer) depends on the unpacked
#    package, outside this workspace, and exercises the public API.
#
# Usage: scripts/test-package.sh   (from anywhere; needs cargo on PATH)
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

version="$(cargo pkgid -p agentbuild-core | sed 's/.*[@#]//')"
echo "== packaging agentbuild-core $version"
# --allow-dirty: also test uncommitted changes when run locally.
cargo package -p agentbuild-core --allow-dirty

package_dir="$root/target/package/agentbuild-core-$version"
for file in README.md LICENSE-MIT LICENSE-APACHE Cargo.toml src/lib.rs; do
  if [[ ! -f "$package_dir/$file" ]]; then
    echo "package is missing $file" >&2
    exit 1
  fi
done
echo "== package contents ok"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp -r "$root/tests/package/rust-consumer/." "$work/"
rm "$work/Cargo.toml.in"
# Cargo accepts forward slashes on every platform.
path="$package_dir"
if command -v cygpath >/dev/null 2>&1; then
  path="$(cygpath -m "$package_dir")"
fi
sed "s|@PACKAGE_DIR@|$path|" "$root/tests/package/rust-consumer/Cargo.toml.in" >"$work/Cargo.toml"

echo "== building a consumer project against the package"
CARGO_TARGET_DIR="$root/target/package-consumer" \
  cargo run --quiet --manifest-path "$work/Cargo.toml"
