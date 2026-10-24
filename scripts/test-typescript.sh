#!/usr/bin/env bash
# The TypeScript SDK as users get it (design 0012 §5, AGENTS.md rule 9):
# builds the release addon and dist/, packs the package with npm, installs
# the tarball into a NEW project, runs a consumer script that uses only the
# public API, then the test suite against the installed package.
#
# Needs Node 18+ and Rust. typescript and @types/node come from npm.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sdk="$root/sdks/typescript"
export CARGO_TARGET_DIR="$root/target/node"

cargo build --release --quiet --manifest-path "$sdk/Cargo.toml"
node "$sdk/scripts/copy-native.js" release
(cd "$sdk" && npm ci --silent --ignore-scripts --no-audit --no-fund && npx tsc -p tsconfig.json)

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
(cd "$sdk" && npm pack --silent --pack-destination "$work" >/dev/null)
tarball="$(ls "$work"/agentbuild-sdk-*.tgz | head -1)"
echo "packed $(basename "$tarball")"

mkdir "$work/consumer"
cd "$work/consumer"
npm init -y >/dev/null
# Zod as a user would install it next to the SDK (the Standard Schema test);
# the version the SDK's devDependencies pin.
zod="zod@$(cd "$sdk" && node -p 'require("./package.json").devDependencies.zod')"
npm install --silent --ignore-scripts --no-audit --no-fund "$tarball" "$zod"
cp "$sdk/examples/consumer.mjs" .
node consumer.mjs
cp -r "$sdk/test" ./test
AGENTBUILD_ERROR_CATALOG="$root/schemas/error-codes.json" AGENTBUILD_SDK=agentbuild-sdk node --test test/*.test.js
