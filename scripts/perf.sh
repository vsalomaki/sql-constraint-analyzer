#!/usr/bin/env bash
#
# Times `sqlca check` and `sqlca fix` (release build) on generated exports in gen-out/: a ~1 GB standard run in the
# separate-files layout, a ~2.4 GB one whose main schema is a single file past 2 GiB, and a smaller one with cascade
# rows, children of bad rows that the fix removes in later rounds. The check runs in end-state and load-order mode; its
# findings must equal the manifest. The fix writes fixed copies beside the exports, and its removed rows, remaining
# findings and row count must be what the manifest implies; its time is also shown against the end-state check's.
# Linux only: peak RSS is read from /proc.
#
# PERF_STANDARD_ROWS, PERF_LARGE_ROWS and PERF_CASCADE_ROWS change the sizes; PERF_RUNS picks the runs.
#
set -euo pipefail
cd "$(dirname "$0")/.."

STANDARD_ROWS=${PERF_STANDARD_ROWS:-750000}
LARGE_ROWS=${PERF_LARGE_ROWS:-1700000}
CASCADE_ROWS=${PERF_CASCADE_ROWS:-200000}
RUNS=${PERF_RUNS:-standard large cascades}

cargo build --quiet --release --features gen --bins
BIN=$PWD/target/release
TMP=$(mktemp -d)
OUT=
trap 'rm -rf "$TMP" "$OUT"' EXIT

# The export for `rows`, `layout` and `cascades` (true or false), generated unless gen-out already holds one with
# those options and a manifest that lists what the fix removes.
export_for() {
    local dir=$1 rows=$2 layout=$3 cascades=$4
    if [[ -f $dir/expected.json ]] && jq -e --argjson rows "$rows" --arg layout "$layout" \
        --argjson cascades "$cascades" \
        '.options.rows == $rows and .options.layout == $layout and .options["omit-commit"] == []
            and (.options.cascades // false) == $cascades and has("fixes")' \
        "$dir/expected.json" > /dev/null; then
        return
    fi
    rm -rf "$dir"
    local flags=()
    [[ $cascades == true ]] && flags+=(--cascades)
    "$BIN/sqlca-gen" --out "$dir" --rows "$rows" --layout "$layout" "${flags[@]}" >&2
}

# Runs the command in `dir` with stdout to $TMP/stdout and stderr to $TMP/stderr; sets NS, PEAK_KB and CODE.
timed() {
    local dir=$1 start pid
    shift
    PEAK_KB=0
    start=$(date +%s%N)
    (cd "$dir" && exec "$@") > "$TMP/stdout" 2> "$TMP/stderr" &
    pid=$!
    # VmHWM is the process's peak resident set so far; it is gone once the process exits.
    while kill -0 "$pid" 2> /dev/null; do
        { while read -r key value _; do
            [[ $key == VmHWM: ]] && PEAK_KB=$value
        done < "/proc/$pid/status"; } 2> /dev/null || true
        sleep 0.02
    done
    CODE=0
    wait "$pid" || CODE=$?
    NS=$(($(date +%s%N) - start))
}

# Fails unless the last timed run exited 1, as it does while findings remain.
exited_with_findings() {
    if [[ $CODE -ne 1 ]]; then
        cat "$TMP/stderr" >&2
        echo "perf: $1 exited $CODE, expected 1 (findings)" >&2
        return 1
    fi
}

# Fails unless the sorted lines of `found` equal those of `wanted`.
same() {
    local wanted=$1 found=$2 what=$3
    if ! cmp -s "$wanted" "$found"; then
        # Not a pipe: under pipefail, diff killed by head's early exit would end the script before the message.
        head -20 < <(diff "$wanted" "$found") >&2
        echo "perf: $what differ from the manifest (< expected, > found)" >&2
        return 1
    fi
}

# One result line; `vs` is the wall time against another run's, or empty.
row() {
    local run=$1 mode=$2 ns=$3 kb=$4 findings=$5 vs=${6:-}
    awk -v run="$run" -v mode="$mode" -v bytes="$BYTES" -v largest="$LARGEST" -v ns="$ns" -v kb="$kb" \
        -v findings="$findings" -v vs="$vs" 'BEGIN {
        s = ns / 1e9
        printf "%-9s %-11s %8.0f MB %9.0f MB %8.2f s %8.1f MB/s %8.0f MiB %8d",
            run, mode, bytes / 1e6, largest / 1e6, s, bytes / 1e6 / s, kb / 1024, findings
        if (vs != "") printf " %8.2fx", ns / vs
        printf "\n"
    }'
}

# Runs the check on `dir` with the extra arguments, prints one result line and fails when the findings differ from
# the manifest. Sets ROWS_IN to the rows the check read.
measure() {
    local dir=$1 run=$2 mode=$3
    shift 3
    timed "$dir" "$BIN/sqlca" check --files-from "$TMP/files" --format json --max-per-constraint 0 -q \
        "$@" --reference 'ref/**/*.sql'
    exited_with_findings "$run $mode"

    local load_order=false
    [[ $mode == load-order ]] && load_order=true
    jq -cS 'select(has("summary") | not)' "$TMP/stdout" | sort > "$TMP/found"
    jq -cS --argjson load_order "$load_order" \
        '.findings[] | select($load_order or .kind != "fk_load_order")' "$dir/expected.json" | sort > "$TMP/wanted"
    same "$TMP/wanted" "$TMP/found" "$run $mode findings"
    ROWS_IN=$(jq 'select(has("summary")) | .summary.rows' "$TMP/stdout")

    row "$run" "$mode" "$NS" "$PEAK_KB" "$(wc -l < "$TMP/found")"
}

# The kinds of finding `fix`'s default policy acts on, as `acted_on` in src/fix.rs decides.
FIXED='def fixed: .kind | IN("fk_missing_parent", "duplicate_key", "not_null");'

# Fails unless every file in $OUT is its input in `dir` with the rows the manifest lists as removed taken out, byte for
# byte. A removal takes its one line through the line end; a row of several lines would fail here.
kept_bytes() {
    local dir=$1 run=$2 file lines
    jq -r '.fixes.removed[] | [.file, .line] | @tsv' "$dir/expected.json" > "$TMP/removed"
    while read -r file; do
        lines=$(awk -F '\t' -v file="$file" '$1 == file { print $2 }' "$TMP/removed")
        if ! LC_ALL=C awk -v lines="$lines" 'BEGIN { n = split(lines, l); for (i = 1; i <= n; i++) gone[l[i]] }
            !(FNR in gone)' "$dir/$file" | cmp - "$OUT/$file" >&2; then
            echo "perf: $run fix wrote $file other than its input without the removed rows" >&2
            return 1
        fi
    done < "$dir/order.txt"
}

# Runs the fix on `dir` into a folder beside the exports, prints one result line with its time against `check_ns`,
# and fails unless it removed the rows the manifest lists and nothing else, left the manifest's other findings, and
# kept every other row byte for byte.
measure_fix() {
    local dir=$1 run=$2 check_ns=$3
    OUT=$(mktemp -d "$PWD/gen-out/perf-fix.XXXXXX")
    timed "$dir" "$BIN/sqlca" fix --files-from "$TMP/files" -q --reference 'ref/**/*.sql' --out "$OUT"
    exited_with_findings "$run fix"
    local report=$OUT/sqlca-report/fix-report.json

    jq -cS '.removed[] | {file, line}' "$report" | sort > "$TMP/found"
    jq -cS '.fixes.removed[] | {file, line}' "$dir/expected.json" | sort > "$TMP/wanted"
    same "$TMP/wanted" "$TMP/found" "$run fix removed rows"
    local removed
    removed=$(wc -l < "$TMP/found")

    # The output's findings name the files fix wrote; `files` pairs each with its input.
    jq -cS '(.files | map({(.output): .input}) | add) as $input | .output.findings[] | .file = $input[.file]' \
        "$report" | sort > "$TMP/found"
    # Each row in these exports is one line, so each removal moves the lines below it in its file up by one, and
    # takes one from the uncommitted rows it is among.
    jq -cS "$FIXED"'
        (.fixes.removed | group_by(.file) | map({key: .[0].file, value: [.[].line]}) | from_entries) as $removed
        | .findings[]
        | select((fixed or .kind == "fk_load_order") | not)
        | .line as $line
        | .line -= ([$removed[.file][]? | select(. < $line)] | length)
        | if .kind == "uncommitted" then .count -= ([$removed[.file][]? | select(. >= $line)] | length) else . end
        ' "$dir/expected.json" | sort > "$TMP/wanted"
    same "$TMP/wanted" "$TMP/found" "$run fix remaining findings"

    local rows_out
    rows_out=$(jq '.output.summary.rows' "$report")
    if [[ $rows_out -ne $((ROWS_IN - removed)) ]]; then
        echo "perf: $run fix left $rows_out rows, expected $ROWS_IN - $removed removed" >&2
        return 1
    fi
    kept_bytes "$dir" "$run"

    row "$run" fix "$NS" "$PEAK_KB" "$(wc -l < "$TMP/found")" "$check_ns"
    rm -rf "$OUT"
}

declare -A ROWS=([standard]=$STANDARD_ROWS [large]=$LARGE_ROWS [cascades]=$CASCADE_ROWS)
declare -A LAYOUT=([standard]=separate [large]=single [cascades]=separate)
declare -A CASCADES=([standard]=false [large]=false [cascades]=true)

printf "%-9s %-11s %11s %12s %10s %13s %11s %9s %9s\n" \
    run mode input "largest file" wall throughput "peak RSS" findings "vs check"
for run in $RUNS; do
    dir=gen-out/perf-${LAYOUT[$run]}-${ROWS[$run]}
    [[ ${CASCADES[$run]} == true ]] && dir+=-cascades
    export_for "$dir" "${ROWS[$run]}" "${LAYOUT[$run]}" "${CASCADES[$run]}"
    grep -v '^ref/' "$dir/order.txt" > "$TMP/files"
    BYTES=$(cd "$dir" && xargs stat -c %s < order.txt | awk '{ s += $1 } END { print s }')
    LARGEST=$(cd "$dir" && xargs stat -c %s < order.txt | sort -n | tail -1)
    if [[ $run == large && $LARGEST -le 2147483648 ]]; then
        echo "perf: the large run's largest file is $LARGEST bytes, not past 2 GiB; raise PERF_LARGE_ROWS" >&2
        exit 1
    fi

    measure "$dir" "$run" end-state
    check_ns=$NS
    measure "$dir" "$run" load-order --load-order
    measure_fix "$dir" "$run" "$check_ns"
done
