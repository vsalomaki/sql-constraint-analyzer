#!/usr/bin/env bash
#
# Oracle ground truth: runs the ignored test that loads generated exports into Oracle Free and compares Oracle's
# rejections with the manifests. The container is kept for the next run; `--stop` removes it.
#
set -euo pipefail
cd "$(dirname "$0")/../.."
# shellcheck source=scripts/oracle/container.sh
source scripts/oracle/container.sh

if [[ "${1:-}" == "--stop" ]]; then
    docker rm -f "$NAME" > /dev/null 2>&1 || true
    exit 0
fi

start_oracle
export SQLCA_ORACLE_CONTAINER="$NAME" SQLCA_ORACLE_PASSWORD="$PASSWORD"
exec cargo nextest run --all-features --run-ignored only oracle_agrees_with_the_manifest "$@"
