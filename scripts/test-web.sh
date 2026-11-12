#!/usr/bin/env bash
# The engine in the browser (design 0021), every layer:
#
#   1. criogenai-rt's browser runtime as WebAssembly on a JavaScript event loop
#      (Node, wasm-bindgen-test-runner): timed tests of tasks, timers, aborts;
#   2. the build: sdks/typescript/scripts/build-web.mjs (engine, glue,
#      bundle, types);
#   3. the TypeScript SDK's whole test suite on the WebAssembly engine
#      (CRIOGEN_ENGINE=wasm; what needs a server is skipped, with the reason);
#   4. the browser tests (sdks/typescript/test/browser) in Chromium, Firefox
#      and WebKit, against the built bundle;
#   5. the package (rule 9): npm pack, installed into a NEW project; an app
#      importing "criogenai/web" is bundled as a user's bundler would and run
#      in Chromium; then the browser tests against the installed files.
#
# Needs Rust with the wasm32-unknown-unknown target, wasm-bindgen-cli at the
# version in sdks/typescript/web/Cargo.lock, Node 18+, and Playwright's
# browsers (npx playwright install chromium firefox webkit, in sdks/typescript).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sdk="$root/sdks/typescript"
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"

echo "== 1. the browser runtime (criogenai-rt) as WebAssembly"
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
  cargo test -p criogenai-rt --target wasm32-unknown-unknown

echo "== 2. the build"
(cd "$sdk" && npm ci --silent --ignore-scripts --no-audit --no-fund && npx tsc -p tsconfig.json && node scripts/build-web.mjs)

echo "== 3. the TypeScript suite on the WebAssembly engine"
(cd "$sdk" && node scripts/test-wasm.mjs)

echo "== 4. the browser tests"
(cd "$sdk" && node test/browser/run.mjs)

echo "== 5. the package"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
(cd "$sdk" && npm pack --silent --pack-destination "$work" >/dev/null)
tarball="$(ls "$work"/criogenai-*.tgz | head -1)"
echo "packed $(basename "$tarball")"
tar -tzf "$tarball" >"$work/contents.txt"
for file in package/dist-web/criogenai.js package/dist-web/criogenai_bg.wasm package/dist-web/types/browser/index.d.ts; do
  grep -qx "$file" "$work/contents.txt" || { echo "the package is missing $file" >&2; exit 1; }
done
mkdir "$work/app"
cp "$root/tests/package/web-consumer/"{app.js,index.html} "$work/app/"
(cd "$work/app" && npm init -y >/dev/null && npm install --silent --ignore-scripts --no-audit --no-fund "$tarball")
node "$root/tests/package/web-consumer/run.mjs" "$work/app" "$sdk"
(cd "$sdk" && node test/browser/run.mjs --files "$work/app/node_modules/criogenai/dist-web")

echo "web ok"
