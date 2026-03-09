#!/bin/bash
set -e

COMPOSE="docker compose -f docker/double-nat/docker-compose.yml"

case "${1:-up}" in
  up)
    echo "Building and starting double-NAT test environment..."
    $COMPOSE up -d --build
    sleep 3
    echo ""
    echo "Topology:"
    echo "  client1 (172.16.1.2) -> nat1 [+50ms] -> net_relay -> nat2 [+50ms] -> client2 (172.16.2.2)"
    echo "  signal server: 10.10.0.10:7878"
    echo ""
    echo "Shell into clients:"
    echo "  docker exec -it double-nat-client1-1 bash"
    echo "  docker exec -it double-nat-client2-1 bash"
    echo ""
    echo "Adjust latency (e.g. 100ms) in docker-compose.yml LATENCY_MS env var, then re-run: $0 up"
    ;;
  down)
    $COMPOSE down
    ;;
  logs)
    $COMPOSE logs -f
    ;;
  *)
    echo "Usage: $0 [up|down|logs]"
    exit 1
    ;;
esac
