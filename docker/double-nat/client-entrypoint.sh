#!/bin/bash
set -e

ip route del default 2>/dev/null || true
ip route add default via "${GW}"

exec "$@"
