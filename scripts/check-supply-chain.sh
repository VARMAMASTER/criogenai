#!/usr/bin/env bash
# Supply-chain guard (run by scripts/ci-local.sh, step `supply`). LiteLLM was
# compromised in 2026 through a CI action referenced by a movable tag, a
# long-lived publishing token, and installs that took whatever was newest.
# This fails when any of those creeps back in:
#
#   1. every `uses:` in .github/workflows is a full 40-character commit hash
#      (a tag or branch can be rewritten);
#   2. a job that installs or builds never has a publishing secret in its
#      step environment (secrets only in steps that run `publish` commands);
#   3. scripts install Python packages only from the hash-locked file, and
#      Node packages only with `npm ci` / `--ignore-scripts`;
#   4. `pull_request_target` is not used (it runs untrusted code with secrets).
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
fail=0
bad() { echo "supply-chain: $*" >&2; fail=1; }

# 1. Actions pinned by commit hash.
while IFS= read -r line; do
  ref="${line##*uses:}"; ref="${ref%%#*}"; ref="$(echo "$ref" | tr -d ' ')"
  case "$ref" in ./*) continue ;; esac
  [[ "$ref" =~ @[0-9a-f]{40}$ ]] || bad "unpinned action: $line"
done < <(grep -rhn "uses:" .github/workflows)

# 4. No pull_request_target.
if grep -rn "pull_request_target" .github/workflows >/dev/null; then
  bad "pull_request_target is used in .github/workflows"
fi

# 2. Secrets only in publish steps: a step with a secret in `env:` must not
# run install/build commands.
python3 - <<'PY' || fail=1
import glob, re, sys
bad = False
for path in glob.glob(".github/workflows/*.yml"):
    text = open(path, encoding="utf-8").read()
    for step in re.split(r"\n      - ", text):
        if "secrets." not in step or "${{ secrets" not in step:
            continue
        for cmd in ("npm install", "npm ci", "pip install", "cargo build", "npx ", "tsc"):
            if cmd in step:
                print(f"supply-chain: {path}: a step with a secret runs `{cmd}`", file=sys.stderr)
                bad = True
sys.exit(1 if bad else 0)
PY

# 3. Locked installs.
if grep -n "pip install" scripts/*.sh website/scripts/*.sh | grep -v -e "--require-hashes" -e "--no-deps" -e "^scripts/check-supply-chain.sh" >/dev/null; then
  grep -n "pip install" scripts/*.sh website/scripts/*.sh | grep -v -e "--require-hashes" -e "--no-deps" -e "^scripts/check-supply-chain.sh" >&2
  bad "pip install without --require-hashes (use scripts/locks/python-tools.txt)"
fi
if grep -n "npm install\|npm ci" scripts/*.sh website/scripts/*.sh scripts/ci-local.sh | grep -v -e "--ignore-scripts" -e "^scripts/check-supply-chain.sh" >/dev/null; then
  grep -n "npm install\|npm ci" scripts/*.sh website/scripts/*.sh scripts/ci-local.sh | grep -v -e "--ignore-scripts" -e "^scripts/check-supply-chain.sh" >&2
  bad "npm install/ci without --ignore-scripts"
fi

[[ $fail == 0 ]] && echo "supply chain ok"
exit $fail
