#!/usr/bin/env bash
# After a release: install `criogenai@<version>` from the public npm registry into a
# new, empty project with each package manager found here, and run a small agent
# (engine load, ready(), a tool call on FakeModel). No keys, no real model.
#
#   bash scripts/smoke-registry.sh 0.1.1          # npm, pnpm, yarn, bun (those installed)
#   SMOKE_MANAGERS="npm pnpm" bash scripts/smoke-registry.sh 0.1.1
set -euo pipefail

version="${1:?usage: smoke-registry.sh <version>}"
managers="${SMOKE_MANAGERS:-npm pnpm yarn bun}"
root="$(cd "$(dirname "$0")/.." && pwd)/target/smoke-registry/$version"
rm -rf "$root"
mkdir -p "$root"

cat > "$root/check.cjs" <<'JS'
const { Agent, FakeModel, ready, tool } = require("criogenai");
const version = require("criogenai/package.json").version;
(async () => {
  const report = await ready();
  if (!report.ok) throw new Error(`engine did not load: ${JSON.stringify(report.engine)}`);
  const add = tool({
    name: "add",
    description: "Adds two numbers.",
    input: { type: "object", properties: { a: { type: "number" }, b: { type: "number" } }, required: ["a", "b"] },
    run: ({ a, b }) => a + b,
  });
  const model = new FakeModel([{ toolCalls: [{ name: "add", args: { a: 2, b: 3 } }] }, "The sum is 5."]);
  const result = await new Agent({ model, tools: [add] }).run("2 + 3?");
  if (result.text !== "The sum is 5." || result.toolCalls !== 1) throw new Error(`unexpected result: ${result.text}`);
  console.log(`ok criogenai@${version} on ${report.engine.platform}-${report.engine.arch}`);
})().catch((error) => { console.error(`FAILED: ${error.message}`); process.exit(1); });
JS

failed=0
tried=0
for pm in $managers; do
  if ! command -v "$pm" >/dev/null 2>&1; then
    echo "skip $pm (not installed)"
    continue
  fi
  tried=$((tried + 1))
  dir="$root/$pm"
  mkdir -p "$dir"
  cp "$root/check.cjs" "$dir/"
  printf '{ "name": "criogenai-smoke-%s", "private": true }\n' "$pm" > "$dir/package.json"
  echo "== $pm"
  (
    cd "$dir"
    case "$pm" in
      npm)  npm install --ignore-scripts --no-audit --no-fund --prefer-online "criogenai@$version" >install.log 2>&1 ;;
      pnpm) pnpm add --ignore-scripts "criogenai@$version" >install.log 2>&1 ;;
      yarn) yarn add --ignore-scripts "criogenai@$version" >install.log 2>&1 ;;
      bun)  bun add --ignore-scripts "criogenai@$version" >install.log 2>&1 ;;
    esac
  ) || { echo "FAILED: $pm install (see $dir/install.log)"; failed=$((failed + 1)); continue; }
  if [ "$pm" = bun ]; then runner=(bun check.cjs); else runner=(node check.cjs); fi
  (cd "$dir" && "${runner[@]}") || failed=$((failed + 1))
done

if [ "$tried" -eq 0 ]; then echo "no package manager found"; exit 1; fi
if [ "$failed" -gt 0 ]; then echo "$failed of $tried failed"; exit 1; fi
echo "all $tried passed"
