#!/usr/bin/env bash
# Tells Bing and Yandex (IndexNow) about every page in the live sitemap, so new
# and changed pages are crawled soon. Bing's index also feeds ChatGPT search and
# Copilot. Google does not use IndexNow (submit the sitemap in Search Console).
# The key is public by design: it is the file website/public/<key>.txt.
#
#   bash scripts/indexnow.sh        # run by deploy-site.sh after each deploy
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
site="https://criogen.vercel.app"
host="criogen.vercel.app"
keyfile="$(ls "$root"/website/public/*.txt 2>/dev/null | grep -E '/[0-9a-f]{32}\.txt$' | head -1 || true)"
if [[ -z $keyfile ]]; then
  echo "indexnow: no key file (website/public/<32 hex>.txt); skipped" >&2
  exit 0
fi
key="$(basename "$keyfile" .txt)"

payload="$(curl -fsS "$site/sitemap.xml" | python -I -c "
import json, re, sys
urls = re.findall(r'<loc>([^<]+)</loc>', sys.stdin.read())
urls += ['$site/llms.txt', '$site/llms-full.txt']
print(json.dumps({'host': '$host', 'key': '$key', 'keyLocation': '$site/$key.txt', 'urlList': urls}))
")"

for endpoint in https://www.bing.com/indexnow https://yandex.com/indexnow; do
  code="$(curl -s -o /dev/null -w '%{http_code}' -X POST "$endpoint" \
    -H 'Content-Type: application/json; charset=utf-8' --data "$payload" || true)"
  # 200 or 202: accepted. Anything else is reported, never fatal to a deploy.
  echo "indexnow: $endpoint -> $code"
done
