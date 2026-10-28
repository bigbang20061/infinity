#!/bin/sh
# First-start database import (docker-entrypoint-initdb.d; runs only while the pgdata
# volume is empty). Loads the per-table dumps in /db/<schema>/<table>.sql.gz, each made with
# `pg_dump --no-owner -t schema.table`, so a fresh install needs neither the client's SDE nor
# sde-import.
#
# Each file loads in one transaction. A table whose foreign keys point at a table not loaded
# yet fails and is retried on the next pass, until everything is in or a pass makes no progress.
set -eu

DUMPS=/db
psql_() { psql -v ON_ERROR_STOP=1 -q -X --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" "$@"; }

pending=$(ls "$DUMPS"/*/*.sql.gz 2>/dev/null || true)
if [ -z "$pending" ]; then
    echo "db-init: no dumps under $DUMPS; leaving the database empty"
    exit 0
fi

for dir in "$DUMPS"/*/; do
    schema=$(basename "$dir")
    ls "$dir"*.sql.gz >/dev/null 2>&1 && psql_ -c "CREATE SCHEMA IF NOT EXISTS \"$schema\""
done

pass=1
while [ -n "$pending" ]; do
    total=$(echo "$pending" | wc -w)
    echo "db-init: pass $pass, $total tables to load"
    failed=""
    for f in $pending; do
        if ! gunzip -c "$f" | psql_ --single-transaction >/dev/null 2>/tmp/db-init.err; then
            failed="$failed $f"
            last="$f: $(head -c 500 /tmp/db-init.err)"
        fi
    done
    left=$(echo "$failed" | wc -w)
    if [ "$left" -eq "$total" ]; then
        echo "db-init: $left tables could not be loaded; last error: $last" >&2
        exit 1
    fi
    pending=$failed
    pass=$((pass + 1))
done
rm -f /tmp/db-init.err
echo "db-init: all dumps loaded"
