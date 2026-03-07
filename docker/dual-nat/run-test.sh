#!/bin/sh
set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
COMPOSE_FILE="$SCRIPT_DIR/docker-compose.yml"

if [ "${1:-}" != "--no-build" ]; then
    docker compose -f "$COMPOSE_FILE" build
fi

docker compose -f "$COMPOSE_FILE" up --abort-on-container-exit --exit-code-from peer_a
code=$?

docker compose -f "$COMPOSE_FILE" down --remove-orphans

exit "$code"
