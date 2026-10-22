#!/usr/bin/env bash
# Shows the local website to someone elsewhere, live: starts `next dev`
# (port 4400, reloads on every change) unless it already runs, and opens an
# ngrok tunnel to it. Prints the public address.
#
# Needs ngrok with an account token (once: `ngrok config add-authtoken …`,
# typed in your own terminal so the token stays out of logs).
#
#   bash scripts/tunnel.sh                       # https://<random>.ngrok-free.app
#   TUNNEL_AUTH=user:long-password bash scripts/tunnel.sh   # ask for a password
#
# Stop it with Ctrl+C (the dev server keeps running; stop it separately).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
port="${PORT:-4400}"

if ! command -v ngrok >/dev/null 2>&1; then
  echo "ngrok is not installed: https://ngrok.com/download (or: winget install Ngrok.Ngrok)" >&2
  exit 1
fi

if ! (echo >"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
  echo "starting the website's dev server on port $port"
  (cd "$root/website" && npm run dev >"$root/target/website-dev.log" 2>&1 &)
  for _ in $(seq 1 60); do
    (echo >"/dev/tcp/127.0.0.1/$port") 2>/dev/null && break
    sleep 1
  done
fi

args=(http "$port" --log stdout --log-format logfmt)
if [[ -n "${TUNNEL_AUTH:-}" ]]; then
  args+=(--basic-auth "$TUNNEL_AUTH")
fi
echo "opening the tunnel; the address follows (the 'url=' field)"
exec ngrok "${args[@]}"
