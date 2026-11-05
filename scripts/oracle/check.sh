#!/usr/bin/env bash
#
# Oracle ground truth: starts Oracle Free in Docker (no published ports; reached only with `docker exec`), waits
# until it is ready, then runs the ignored test that loads generated exports into it and compares Oracle's
# rejections with the manifests. The container is kept for the next run; `--stop` removes it.
#
set -euo pipefail

# The 23 tags now ship Oracle 26ai Free; the digest pins the exact build.
IMAGE="gvenzl/oracle-free:23-slim-faststart@sha256:f5ff19033860d662c821cb04eb10483fa94f14f78eae252d054291ea07028093"
NAME="${SQLCA_ORACLE_CONTAINER:-sqlca-oracle}"
# A throwaway password for a local container that publishes no port.
PASSWORD="${SQLCA_ORACLE_PASSWORD:-sqlca-local}"

if [[ "${1:-}" == "--stop" ]]; then
    docker rm -f "$NAME" > /dev/null 2>&1 || true
    exit 0
fi

if ! docker inspect "$NAME" > /dev/null 2>&1; then
    echo "oracle-check: starting $NAME (first start pulls about 1.3 GB)" >&2
    docker run -d --name "$NAME" -e ORACLE_PASSWORD="$PASSWORD" "$IMAGE" > /dev/null
else
    # Reuse the container with the password it was created with.
    PASSWORD="$(docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$NAME" | sed -n 's/^ORACLE_PASSWORD=//p')"
    if [[ "$(docker inspect -f '{{.State.Running}}' "$NAME")" != true ]]; then
        docker start "$NAME" > /dev/null
    fi
fi

for _ in $(seq 1 150); do
    docker exec "$NAME" /opt/oracle/healthcheck.sh > /dev/null 2>&1 && break
    sleep 2
done
if ! docker exec "$NAME" /opt/oracle/healthcheck.sh > /dev/null 2>&1; then
    echo "oracle-check: $NAME is not ready after 5 minutes; see docker logs $NAME" >&2
    exit 1
fi

export SQLCA_ORACLE_CONTAINER="$NAME" SQLCA_ORACLE_PASSWORD="$PASSWORD"
exec cargo nextest run --all-features --run-ignored only oracle_agrees_with_the_manifest "$@"
