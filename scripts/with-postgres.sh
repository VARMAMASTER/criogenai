#!/usr/bin/env bash
# Runs a command against a throwaway Postgres server, for the Postgres
# tests: a new cluster in a temporary directory, listening on 127.0.0.1
# only, trust auth, deleted afterwards. The command finds it in
# CRIOGEN_TEST_POSTGRES_URL (tests create and drop their own databases).
#
#   bash scripts/with-postgres.sh cargo test --workspace --all-features
#
# Needs initdb and pg_ctl (PostgreSQL 13+): in the directory $PG_BIN, on
# PATH, or in ~/.criogen/postgres/bin, where scripts/get-postgres.sh puts
# portable binaries (no install, no service).
# With openssl on PATH, the server also accepts TLS, with a self-signed
# certificate for 127.0.0.1 whose path is in CRIOGEN_TEST_POSTGRES_CA.
# If CRIOGEN_TEST_POSTGRES_URL is already set, the command runs against
# that server instead and nothing is started.
#
# Exit code: the command's; 3 if no Postgres binaries were found.
set -euo pipefail

if [[ $# -eq 0 ]]; then
  echo "usage: $0 command [args...]" >&2
  exit 2
fi

if [[ -n ${CRIOGEN_TEST_POSTGRES_URL:-} ]]; then
  exec "$@"
fi

bin=""
portable="${PG_HOME:-$HOME/.criogen/postgres}/bin"
if [[ -n ${PG_BIN:-} ]]; then
  bin="${PG_BIN%/}/"
elif command -v pg_ctl >/dev/null 2>&1; then
  bin=""
elif [[ -d $portable ]]; then
  bin="$portable/"
else
  echo "no Postgres binaries: run scripts/get-postgres.sh, or set PG_BIN" >&2
  exit 3
fi
if ! "${bin}pg_ctl" --version >/dev/null 2>&1; then
  echo "pg_ctl not found or not working in ${bin:-PATH}" >&2
  exit 3
fi

data="$(mktemp -d)"
port="${PG_PORT:-$((54000 + RANDOM % 1000))}"
stop() {
  "${bin}pg_ctl" -D "$data" -m immediate -w stop >/dev/null 2>&1 || true
  rm -rf "$data"
}
trap stop EXIT

"${bin}initdb" -D "$data" -U postgres --auth=trust -E UTF8 --no-locale >"$data.initdb.log" 2>&1 || {
  cat "$data.initdb.log" >&2
  rm -f "$data.initdb.log"
  exit 1
}
rm -f "$data.initdb.log"
# Enough connections for the tests' parallel databases and thread locks.
options="-p $port -h 127.0.0.1 -c max_connections=300 -c fsync=off"
tls=""
# A throwaway CA and a server certificate it signs (a self-signed CA
# certificate used as the server's is rightly refused by verify-full).
make_certs() (
  cd "$data" || exit 1
  export MSYS_NO_PATHCONV=1
  openssl req -new -x509 -days 2 -nodes -subj "/CN=criogenai test CA" \
    -keyout ca.key -out ca.crt &&
    openssl req -new -nodes -subj "/CN=localhost" -keyout server.key -out server.csr &&
    printf 'basicConstraints=CA:FALSE\nsubjectAltName=IP:127.0.0.1,DNS:localhost\n' >server.ext &&
    openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -days 2 \
      -extfile server.ext -out server.crt &&
    chmod 600 server.key
)
if command -v openssl >/dev/null 2>&1 && make_certs >/dev/null 2>&1; then
  tls=yes
fi
if [[ -n $tls ]] && "${bin}pg_ctl" -D "$data" -l "$data/server.log" -w \
  -o "$options -c ssl=on" start >/dev/null 2>&1; then
  ca="$data/ca.crt"
  # A path the (native) test programs can open.
  if command -v cygpath >/dev/null 2>&1; then
    ca="$(cygpath -m "$ca")"
  fi
  export CRIOGEN_TEST_POSTGRES_CA="$ca"
else
  "${bin}pg_ctl" -D "$data" -l "$data/server.log" -w -o "$options" start >/dev/null
  tls=""
fi

export CRIOGEN_TEST_POSTGRES_URL="postgres://postgres@127.0.0.1:$port/postgres"
echo "Postgres $("${bin}pg_ctl" --version | awk '{print $NF}') on port $port${tls:+ (TLS on)}"
status=0
"$@" || status=$?
exit "$status"
