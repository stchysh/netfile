#!/bin/bash
set -e

echo 1 > /proc/sys/net/ipv4/ip_forward

iptables -t nat -A POSTROUTING -s "${INTERNAL_NET}" -o "${EXTERNAL_IF}" -j MASQUERADE
iptables -A FORWARD -i eth0 -o "${EXTERNAL_IF}" -j ACCEPT
iptables -A FORWARD -i "${EXTERNAL_IF}" -o eth0 -m state --state RELATED,ESTABLISHED -j ACCEPT

if [ -n "${LATENCY_MS}" ] && [ "${LATENCY_MS}" -gt 0 ]; then
    tc qdisc add dev "${EXTERNAL_IF}" root netem delay "${LATENCY_MS}ms" 5ms distribution normal
fi

exec "$@"
