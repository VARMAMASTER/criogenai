#!/usr/bin/env bash
# Publishes the pilot package built by scripts/pilot-package.sh to GitHub
# Packages (private, for the repository's owner and anyone given access).
#
#   bash scripts/pilot-publish.sh --dry-run     # what would be sent, nothing sent
#   bash scripts/pilot-publish.sh               # publishes
#
# The token is `GITHUB_PACKAGES_TOKEN` (environment, or ~/.agentbuild-keys.env),
# else the GitHub CLI's login (`gh auth login`, with the write:packages scope:
# `gh auth refresh -s write:packages`). It reaches npm through an environment
# variable that an .npmrc line expands, so it is never written to a file or
# printed.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tarball="$(ls "$root"/target/pilot/*.tgz 2>/dev/null | head -1 || true)"
[[ -n $tarball ]] || { echo "no tarball: run scripts/pilot-package.sh first" >&2; exit 1; }

keys="${AGENTBUILD_KEYS_FILE:-$HOME/.agentbuild-keys.env}"
if [[ -z ${GITHUB_PACKAGES_TOKEN:-} && -f $keys ]]; then
  set -a
  # shellcheck disable=SC1090
  . "$keys" >/dev/null 2>&1
  set +a
fi
if [[ -z ${GITHUB_PACKAGES_TOKEN:-} ]] && command -v gh >/dev/null 2>&1; then
  GITHUB_PACKAGES_TOKEN="$(gh auth token 2>/dev/null || true)"
fi
[[ -n ${GITHUB_PACKAGES_TOKEN:-} ]] || {
  echo "no token: set GITHUB_PACKAGES_TOKEN in $keys, or log in with the GitHub CLI" >&2
  exit 1
}
export GITHUB_PACKAGES_TOKEN

npmrc="$(mktemp)"
trap 'rm -f "$npmrc"' EXIT
# The literal text `${GITHUB_PACKAGES_TOKEN}`: npm expands it from the environment.
printf '%s\n' '//npm.pkg.github.com/:_authToken=${GITHUB_PACKAGES_TOKEN}' '@varmamaster:registry=https://npm.pkg.github.com' > "$npmrc"

echo "publishing $(basename "$tarball") as: $(npm whoami --userconfig "$npmrc" --registry https://npm.pkg.github.com)"
npm publish "$tarball" --userconfig "$npmrc" --registry https://npm.pkg.github.com --access restricted "$@"
