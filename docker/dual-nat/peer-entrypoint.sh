#!/bin/sh
set -eu

if [ -n "${GATEWAY:-}" ]; then
    echo "peer-entrypoint: routes before update"
    ip route
    ip route replace default via "$GATEWAY"
    echo "peer-entrypoint: routes after update"
    ip route
fi

exec /usr/local/bin/test-dual-nat-peer
