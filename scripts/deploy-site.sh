#!/usr/bin/env bash
# Deploys the built website (website/out) to Vercel production: project
# `criogen`, team `varmamasters-projects`, https://criogenai.com.
#
#   bash scripts/deploy-site.sh          # builds first if out/ is missing
#
# Why it looks like this:
#  - The site is a static export, and its build reads ../schemas and ../docs,
#    so Vercel cannot build it from the `website/` folder: we build here and
#    upload out/.
#  - out/ is copied outside the repository first. Inside it, the Vercel CLI
#    attaches the git commit's author, and the Hobby plan blocks a deployment
#    whose commit author is not a team member (state BLOCKED).
#  - The CLI version is pinned (never a floating `latest`).
# Needs `vercel login` once on this machine (or VERCEL_TOKEN in the environment).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ ! -d $root/website/out ]]; then
  (cd "$root/website" && npm run build)
fi
if grep -rIl --include='*.json' --include='*.html' --include='*.txt' -E 'sk-or-v1-|AIza[0-9A-Za-z_-]{20,}|sk-proj-' "$root/website/out" >/dev/null 2>&1; then
  echo "refusing to deploy: something in website/out looks like an API key" >&2
  exit 1
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
cp -r "$root/website/out" "$stage/site"
cd "$stage"
npx --yes vercel@59.26.0 deploy site --prod --yes --name criogen --scope varmamasters-projects

# Tell Bing and Yandex about the pages (never fails the deploy).
bash "$root/scripts/indexnow.sh" || true
