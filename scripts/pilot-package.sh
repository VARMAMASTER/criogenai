#!/usr/bin/env bash
# Builds the private pilot package for GitHub Packages and tests it by
# installing it into a new project (nothing is published here).
#
#   bash scripts/pilot-package.sh
#
# The pilot is `@varmamaster/sdk` (a GitHub Packages scope must be the
# owner's name), for this machine's platform only: the addon in it is the one
# just built, so `os` and `cpu` stop it from installing where it cannot run.
# The public package stays `criogenai` (RELEASING.md). Output:
# target/pilot/<name>-<version>.tgz.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sdk="$root/sdks/typescript"
out="$root/target/pilot"
export CARGO_TARGET_DIR="$root/target/node"
NAME="${PILOT_NAME:-@varmamaster/sdk}"
VERSION="${PILOT_VERSION:-0.1.0-pilot.1}"

echo "== build the addon and the TypeScript"
cargo build --release --quiet --manifest-path "$sdk/Cargo.toml"
node "$sdk/scripts/copy-native.js" release
(cd "$sdk" && npm ci --silent --ignore-scripts --no-audit --no-fund && npx tsc -p tsconfig.json)

echo "== stage $NAME@$VERSION"
rm -rf "$out"
mkdir -p "$out/package/native"
cp -r "$sdk/dist" "$out/package/dist"
cp "$sdk/native/criogenai.node" "$out/package/native/criogenai.node"
cp "$sdk/README.md" "$sdk/LICENSE" "$out/package/"
NAME="$NAME" VERSION="$VERSION" SDK="$sdk" OUT="$out/package/package.json" node -e '
  const fs = require("fs");
  const pkg = JSON.parse(fs.readFileSync(process.env.SDK + "/package.json", "utf8"));
  const platform = { win32: "win32", linux: "linux", darwin: "darwin" }[process.platform];
  const cpu = { x64: "x64", arm64: "arm64" }[process.arch];
  if (!platform || !cpu) throw new Error(`no pilot build for ${process.platform}/${process.arch}`);
  const out = {
    name: process.env.NAME,
    version: process.env.VERSION,
    description: pkg.description + " (private pilot)",
    license: pkg.license,
    main: pkg.main,
    types: pkg.types,
    engines: pkg.engines,
    os: [platform],
    cpu: [cpu],
    files: ["dist", "native/criogenai.node", "README.md", "LICENSE"],
    repository: { type: "git", url: "git+https://github.com/VARMAMASTER/criogen.git" },
    publishConfig: { registry: "https://npm.pkg.github.com", access: "restricted" },
  };
  fs.writeFileSync(process.env.OUT, JSON.stringify(out, null, 2) + "\n");
'
(cd "$out/package" && npm pack --silent --pack-destination "$out" >/dev/null)
tarball="$(ls "$out"/*.tgz | head -1)"
echo "packed $(basename "$tarball") ($(du -h "$tarball" | cut -f1))"

echo "== install it into a new project and use it"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"
npm init -y >/dev/null
zod="zod@$(cd "$sdk" && node -p 'require("./package.json").devDependencies.zod')"
npm install --silent --ignore-scripts --no-audit --no-fund "$tarball" "$zod"
# The consumer and the tests import `criogenai`: point them at the pilot.
sed "s#criogenai#$NAME#g" "$sdk/examples/consumer.mjs" > consumer.mjs
node consumer.mjs
cp -r "$sdk/test" ./test
CRIOGEN_ERROR_CATALOG="$root/schemas/error-codes.json" CRIOGEN_SDK="$NAME" node --test test/*.test.js \
  | grep -E "^# (pass|fail)"
echo "pilot package ok: $tarball"
