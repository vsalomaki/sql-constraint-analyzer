#!/usr/bin/env bash
#
# Times `sqlca check` (release build) on generated exports in gen-out/: a ~1 GB standard run in the separate-files
# layout, and a ~2.4 GB one whose main schema is a single file past 2 GiB. Each runs in end-state and load-order
# mode; the findings must equal the manifest. Linux only: peak RSS is read from /proc.
#
# PERF_STANDARD_ROWS and PERF_LARGE_ROWS change the sizes; PERF_RUNS=standard skips the large run.
#
set -euo pipefail
cd "$(dirname "$0")/.."

STANDARD_ROWS=${PERF_STANDARD_ROWS:-750000}
LARGE_ROWS=${PERF_LARGE_ROWS:-1700000}
RUNS=${PERF_RUNS:-standard large}

cargo build --quiet --release --features gen --bins
BIN=$PWD/target/release
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

# The export for `rows` and `layout`, generated unless gen-out already holds one with those options.
export_for() {
    local dir=$1 rows=$2 layout=$3
    if [[ -f $dir/expected.json ]] && jq -e --argjson rows "$rows" --arg layout "$layout" \
        '.options.rows == $rows and .options.layout == $layout' "$dir/expected.json" > /dev/null; then
        return
    fi
    rm -rf "$dir"
    "$BIN/sqlca-gen" --out "$dir" --rows "$rows" --layout "$layout" >&2
}

# Runs the check on `dir` with the extra arguments, prints one result line and fails when the findings differ from
# the manifest.
measure() {
    local dir=$1 run=$2 mode=$3
    shift 3
    grep -v '^ref/' "$dir/order.txt" > "$TMP/files"
    local bytes largest
    bytes=$(cd "$dir" && xargs stat -c %s < order.txt | awk '{ s += $1 } END { print s }')
    largest=$(cd "$dir" && xargs stat -c %s < order.txt | sort -n | tail -1)
    if [[ $run == large && $largest -le 2147483648 ]]; then
        echo "perf: the large run's largest file is $largest bytes, not past 2 GiB; raise PERF_LARGE_ROWS" >&2
        return 1
    fi

    local start end pid peak=0 code
    start=$(date +%s%N)
    (cd "$dir" && exec "$BIN/sqlca" check --files-from "$TMP/files" --format json --max-per-constraint 0 -q \
        "$@" --reference 'ref/**/*.sql') > "$TMP/found.jsonl" 2> "$TMP/stderr" &
    pid=$!
    # VmHWM is the process's peak resident set so far; it is gone once the process exits.
    while kill -0 "$pid" 2> /dev/null; do
        { while read -r key value _; do
            [[ $key == VmHWM: ]] && peak=$value
        done < "/proc/$pid/status"; } 2> /dev/null || true
        sleep 0.02
    done
    code=0
    wait "$pid" || code=$?
    end=$(date +%s%N)

    if [[ $code -ne 1 ]]; then
        cat "$TMP/stderr" >&2
        echo "perf: $run $mode exited $code, expected 1 (findings)" >&2
        return 1
    fi
    local load_order=false
    [[ $mode == load-order ]] && load_order=true
    jq -cS 'select(has("summary") | not)' "$TMP/found.jsonl" | sort > "$TMP/found"
    jq -cS --argjson load_order "$load_order" \
        '.findings[] | select($load_order or .kind != "fk_load_order")' "$dir/expected.json" | sort > "$TMP/wanted"
    if ! cmp -s "$TMP/found" "$TMP/wanted"; then
        diff "$TMP/wanted" "$TMP/found" | head -20 >&2
        echo "perf: $run $mode findings differ from the manifest (< expected, > found)" >&2
        return 1
    fi

    awk -v run="$run" -v mode="$mode" -v bytes="$bytes" -v largest="$largest" -v ns=$((end - start)) \
        -v kb="$peak" -v findings="$(wc -l < "$TMP/found")" 'BEGIN {
        s = ns / 1e9
        printf "%-9s %-11s %8.0f MB %9.0f MB %8.2f s %8.1f MB/s %8.0f MiB %8d\n",
            run, mode, bytes / 1e6, largest / 1e6, s, bytes / 1e6 / s, kb / 1024, findings
    }'
}

declare -A ROWS=([standard]=$STANDARD_ROWS [large]=$LARGE_ROWS)
declare -A LAYOUT=([standard]=separate [large]=single)

printf "%-9s %-11s %11s %12s %10s %13s %11s %9s\n" \
    run mode input "largest file" wall throughput "peak RSS" findings
for run in $RUNS; do
    dir=gen-out/perf-${LAYOUT[$run]}-${ROWS[$run]}
    export_for "$dir" "${ROWS[$run]}" "${LAYOUT[$run]}"
    measure "$dir" "$run" end-state
    measure "$dir" "$run" load-order --load-order
done
