#!/usr/bin/env bash
#
# Writes tests/fixtures/samples/oracle/sqlcl/: the HR and CO sample fixtures loaded into Oracle Free, then exported
# again with SQLcl in SQL Developer's separate-files layout, one folder per schema with its order.txt and
# counts.csv. Run `make sample-fixtures` first when the sample commit changes.
#
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck source=scripts/oracle/container.sh
source scripts/oracle/container.sh

SAMPLES=tests/fixtures/samples/oracle
OUT="$SAMPLES/sqlcl"
SCHEMAS=(hr co)

start_oracle
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for s in "${SCHEMAS[@]}"; do
    cp -r "$SAMPLES/$s" "$work/$s"
done
{
    echo "WHENEVER SQLERROR CONTINUE"
    echo "SET FEEDBACK OFF"
    for s in "${SCHEMAS[@]}"; do
        echo "DROP USER $s CASCADE;"
        echo "CREATE USER $s IDENTIFIED BY $s QUOTA UNLIMITED ON USERS;"
    done
    echo "WHENEVER SQLERROR EXIT FAILURE"
    for s in "${SCHEMAS[@]}"; do
        echo "ALTER SESSION SET CURRENT_SCHEMA = $s;"
        echo "@@$s/${s}_create.sql"
        echo "@@$s/${s}_populate.sql"
    done
    echo "ALTER SESSION SET CURRENT_SCHEMA = SYSTEM;"
    for s in "${SCHEMAS[@]}"; do
        echo "@/scripts/sample-sqlcl-export.sql ${s^^} $s"
    done
    echo "EXIT"
} > "$work/load.sql"

rm -rf "$OUT"
for s in "${SCHEMAS[@]}"; do
    for dir in tables sequences indexes data ref_constraints; do
        mkdir -p "$OUT/$s/$dir"
    done
done
docker run --rm --network "container:$NAME" --user "$(id -u):$(id -g)" -e HOME=/tmp \
    -v "$work:/work" -v "$PWD/scripts/oracle:/scripts:ro" -v "$PWD/$OUT:/out" -w /work \
    "$SQLCL" -s "system/\"$PASSWORD\"@//localhost/FREEPDB1" @load.sql > "$work/sqlcl.log" 2>&1 \
    || { cat "$work/sqlcl.log" >&2; exit 1; }

# The order SQL Developer's separate files load in: tables with their keys, the rows, then the foreign keys.
for s in "${SCHEMAS[@]}"; do
    find "$OUT/$s" -type d -empty -delete
    for dir in tables sequences indexes data ref_constraints; do
        if [[ -d "$OUT/$s/$dir" ]]; then (cd "$OUT/$s" && LC_ALL=C ls "$dir"/*.sql); fi
    done > "$OUT/$s/order.txt"
done
echo "sample-sqlcl-fixtures: wrote $OUT" >&2
