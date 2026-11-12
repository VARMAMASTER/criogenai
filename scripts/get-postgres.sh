#!/usr/bin/env bash
# Downloads portable PostgreSQL binaries for the Postgres tests, so they
# run without installing a server, a service or Docker. Nothing outside the
# target directory changes.
#
#   bash scripts/get-postgres.sh        # into ~/.criogen/postgres
#   PG_HOME=/some/dir bash scripts/get-postgres.sh
#
# Then scripts/with-postgres.sh and scripts/ci-local.sh find them there.
#
# Source: the zonky "embedded-postgres-binaries" builds of PostgreSQL on
# Maven Central, checked against Maven Central's SHA-256. PostgreSQL is
# under the PostgreSQL License; nothing downloaded here is committed or
# shipped with CriogenAI.
set -euo pipefail

version="${PG_VERSION:-18.6.0}"
home="${PG_HOME:-$HOME/.criogen/postgres}"

case "$(uname -s)-$(uname -m)" in
  MINGW* | MSYS* | CYGWIN*) platform=windows-amd64 ;;
  Linux-x86_64) platform=linux-amd64 ;;
  Linux-aarch64 | Linux-arm64) platform=linux-arm64v8 ;;
  Darwin-x86_64) platform=darwin-amd64 ;;
  Darwin-arm64) platform=darwin-arm64v8 ;;
  *)
    echo "no portable PostgreSQL build for $(uname -s) $(uname -m); install PostgreSQL and set PG_BIN" >&2
    exit 1
    ;;
esac

if [[ -x "$home/bin/pg_ctl" || -x "$home/bin/pg_ctl.exe" ]]; then
  echo "already there: $home ($("$home/bin/pg_ctl" --version))"
  exit 0
fi

artifact="embedded-postgres-binaries-$platform"
url="https://repo1.maven.org/maven2/io/zonky/test/postgres/$artifact/$version/$artifact-$version.jar"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "downloading PostgreSQL $version ($platform)"
curl -fsSL "$url" -o "$work/pg.jar"
expected="$(curl -fsSL "$url.sha256" | awk '{print $1}')"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$work/pg.jar" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$work/pg.jar" | awk '{print $1}')"
fi
if [[ "$expected" != "$actual" ]]; then
  echo "checksum mismatch: expected $expected, got $actual" >&2
  exit 1
fi

mkdir -p "$work/jar" "$home"
(cd "$work/jar" && unzip -q ../pg.jar)
tar -xJf "$work"/jar/*.txz -C "$home"
echo "installed: $home ($("$home/bin/pg_ctl" --version))"
