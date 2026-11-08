#!/usr/bin/env bash
#
# Writes tests/fixtures/samples/oracle/: Oracle's HR and CO sample schemas (oracle-samples/db-sample-schemas, MIT) at
# a pinned commit. The create scripts are copied byte for byte. The populate scripts lose only their PL/SQL wrapper:
# the BEGIN, END; and / lines around each table's INSERTs, and in CO the DECLARE that builds product 4's JSON in a
# variable, since in SQL UTL_RAW.CAST_TO_RAW returns at most 2000 bytes. Product 4 keeps as many of its reviews, in
# order, as fit in 2000 bytes. Everything else, the INSERTs included, stays as Oracle wrote it.
#
set -euo pipefail
cd "$(dirname "$0")/../.."

COMMIT=6660bad68c07bd143430ace58565b3f727e17263
BASE="https://raw.githubusercontent.com/oracle-samples/db-sample-schemas/$COMMIT"
OUT=tests/fixtures/samples/oracle

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fetch() { curl -fsSL --retry 3 -o "$work/$(basename "$1")" "$BASE/$1"; }
for f in LICENSE.txt human_resources/hr_create.sql human_resources/hr_populate.sql \
    customer_orders/co_create.sql customer_orders/co_populate.sql; do
    fetch "$f"
done

# Drops the lines that are only BEGIN, END; or /.
unwrap() { grep -v -E '^[[:space:]]*(BEGIN|END;|/)[[:space:]]*$' "$1"; }

# Product 4's JSON, from the two quoted parts its PL/SQL assignment joins with chr(10).
json="$(sed -n "/^  prod_details := '/,/';\$/p" "$work/co_populate.sql" |
    sed -E "s/^  prod_details := '//; s/' \|\| chr\(10\) \|\|\$//; s/^'//; s/';\$//" | tr -d '\n' | sed "s/''/'/g" |
    jq -c 'def fit: if (tojson | utf8bytelength) <= 2000 then . else .reviews |= .[:-1] | fit end; fit')"
literal="$(printf '%s' "$json" | sed "s/'/''/g")"

mkdir -p "$OUT/hr" "$OUT/co"
cp "$work/LICENSE.txt" "$OUT/LICENSE.txt"
cp "$work/hr_create.sql" "$OUT/hr/hr_create.sql"
cp "$work/co_create.sql" "$OUT/co/co_create.sql"
unwrap "$work/hr_populate.sql" > "$OUT/hr/hr_populate.sql"
unwrap "$work/co_populate.sql" |
    LITERAL="$literal" awk '
        /^DECLARE[[:space:]]*$/ || /^  prod_details VARCHAR2\(32767\);/ { next }
        /^  -- split over two lines as 32k varchar2/ { next }
        /^  prod_details := / { skipping = 1 }
        skipping { if (/'"'"';$/) skipping = 0; next }
        /CAST_TO_RAW\( prod_details \)/ {
            i = index($0, "prod_details")
            $0 = substr($0, 1, i - 1) "'"'"'" ENVIRON["LITERAL"] "'"'"'" substr($0, i + length("prod_details"))
        }
        { print }
    ' > "$OUT/co/co_populate.sql"

for f in hr/hr_populate.sql co/co_populate.sql; do
    grep -q -E 'prod_details|^[[:space:]]*(BEGIN|END;|DECLARE)[[:space:]]*$' "$OUT/$f" &&
        { echo "$f still has PL/SQL in it" >&2; exit 1; }
done
echo "wrote $OUT at $COMMIT"
