# shellcheck shell=bash
# Sourced by the scripts in this folder: starts or reuses the Oracle Free container and waits until it is ready.
# Sets NAME and PASSWORD. The container publishes no port; it is reached only with `docker exec` and
# `docker run --network container:$NAME`.

# The 23 tags now ship Oracle 26ai Free; the digest pins the exact build.
IMAGE="gvenzl/oracle-free:23-slim-faststart@sha256:f5ff19033860d662c821cb04eb10483fa94f14f78eae252d054291ea07028093"
# SQLcl, for the scripts that export what Oracle holds.
SQLCL="container-registry.oracle.com/database/sqlcl:latest@sha256:7f1854c7a95c5a0eddf93c6bce1c2d441fab980384a061297e865a5a3ce55d2f"
NAME="${SQLCA_ORACLE_CONTAINER:-sqlca-oracle}"
# A throwaway password for a local container that publishes no port.
PASSWORD="${SQLCA_ORACLE_PASSWORD:-sqlca-local}"

start_oracle() {
    if ! docker inspect "$NAME" > /dev/null 2>&1; then
        echo "oracle: starting $NAME (first start pulls about 1.3 GB)" >&2
        docker run -d --name "$NAME" -e ORACLE_PASSWORD="$PASSWORD" "$IMAGE" > /dev/null
    else
        # Reuse the container with the password it was created with.
        PASSWORD="$(docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$NAME" | sed -n 's/^ORACLE_PASSWORD=//p')"
        if [[ "$(docker inspect -f '{{.State.Running}}' "$NAME")" != true ]]; then
            docker start "$NAME" > /dev/null
        fi
    fi
    for _ in $(seq 1 150); do
        docker exec "$NAME" /opt/oracle/healthcheck.sh > /dev/null 2>&1 && return 0
        sleep 2
    done
    echo "oracle: $NAME is not ready after 5 minutes; see docker logs $NAME" >&2
    return 1
}
