#!/usr/bin/env bash
#
# Writes tests/fixtures/sqlcl/: a generated export loaded into Oracle Free, which rejects the planted bad rows,
# then exported again with SQLcl the way SQL Developer exports. The parser meets files the generator didn't write,
# with no violations in them. Review the diff before committing it: rows whose timestamp came from SYSTIMESTAMP
# change on every run.
#
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck source=scripts/oracle/container.sh
source scripts/oracle/container.sh

OUT=tests/fixtures/sqlcl

start_oracle
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cargo run -q -F gen --bin sqlca-gen -- --out "$work/export" --rows 20 --no-deferred-violations \
    --noise sqldev,plsql-trigger,nextval,multiline-strings,embedded-delimiters,q-quote

{
    echo "WHENEVER SQLERROR CONTINUE"
    echo "SET DEFINE OFF"
    echo "SET FEEDBACK OFF"
    echo "ALTER SESSION SET NLS_DATE_LANGUAGE=AMERICAN;"
    echo "DROP USER APP CASCADE;"
    echo "DROP USER REF CASCADE;"
    echo "CREATE USER APP IDENTIFIED BY app QUOTA UNLIMITED ON USERS;"
    echo "CREATE USER REF IDENTIFIED BY ref QUOTA UNLIMITED ON USERS;"
    grep '^ref/' "$work/export/order.txt" | sed 's#^#@@export/#'
    echo "GRANT REFERENCES ON REF.COUNTRIES TO APP;"
    grep '^app/' "$work/export/order.txt" | sed 's#^#@@export/#'
    echo "COMMIT;"
    # The load expects errors from the planted rows; the export must not have any.
    echo "WHENEVER SQLERROR EXIT FAILURE"
    echo "@/scripts/sqlcl-export.sql"
    echo "EXIT"
} > "$work/load.sql"

rm -rf "$OUT"
for dir in ref/tables ref/data app/sequences app/tables app/indexes app/triggers app/data app/ref_constraints; do
    mkdir -p "$OUT/$dir"
done
docker run --rm --network "container:$NAME" --user "$(id -u):$(id -g)" -e HOME=/tmp \
    -v "$work:/work" -v "$PWD/scripts/oracle:/scripts:ro" -v "$PWD/$OUT:/out" -w /work \
    "$SQLCL" -s "system/\"$PASSWORD\"@//localhost/FREEPDB1" @load.sql > "$work/sqlcl.log" 2>&1 \
    || { cat "$work/sqlcl.log" >&2; exit 1; }

# The order SQL Developer's separate files load in: tables with their keys, the rows, then the foreign keys.
cat > "$OUT/order.txt" << 'EOF'
ref/tables/COUNTRIES.sql
ref/data/COUNTRIES.sql
app/sequences/AUDIT_SEQ.sql
app/tables/DEPARTMENTS.sql
app/tables/EMPLOYEES.sql
app/tables/PROJECTS.sql
app/tables/ASSIGNMENTS.sql
app/tables/TIMESHEETS.sql
app/tables/LEGACY_NOTES.sql
app/tables/AuditLog.sql
app/indexes/EMPLOYEES_EMAIL_UIX.sql
app/triggers/AUDITLOG_BI.sql
app/data/DEPARTMENTS.sql
app/data/EMPLOYEES.sql
app/data/PROJECTS.sql
app/data/ASSIGNMENTS.sql
app/data/TIMESHEETS.sql
app/data/LEGACY_NOTES.sql
app/data/AuditLog.sql
app/ref_constraints/DEPARTMENTS.sql
app/ref_constraints/EMPLOYEES.sql
app/ref_constraints/PROJECTS.sql
app/ref_constraints/ASSIGNMENTS.sql
app/ref_constraints/TIMESHEETS.sql
app/ref_constraints/LEGACY_NOTES.sql
app/ref_constraints/AuditLog.sql
EOF
echo "sqlcl-fixtures: wrote $OUT" >&2
