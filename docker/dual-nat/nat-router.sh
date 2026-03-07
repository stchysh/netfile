#!/bin/sh
set -eu

WAN_IF="${1:-}"
LAN_IF="${2:-}"

if [ -z "$WAN_IF" ]; then
    WAN_IF="$(ip -4 route show default | awk '{print $5; exit}')"
fi

if [ -z "$LAN_IF" ]; then
    LAN_IF="$(ip -o -4 addr show | awk -v wan="$WAN_IF" '$2 != "lo" && $2 != wan {print $2; exit}')"
fi

if [ -z "$WAN_IF" ] || [ -z "$LAN_IF" ]; then
    echo "failed to detect interfaces: WAN=$WAN_IF LAN=$LAN_IF" >&2
    exit 1
fi

if [ -n "${EXT_IP:-}" ]; then
    iptables -t nat -A POSTROUTING -o "$WAN_IF" -j SNAT --to-source "$EXT_IP"
else
    iptables -t nat -A POSTROUTING -o "$WAN_IF" -j MASQUERADE
fi
iptables -A FORWARD -i "$LAN_IF" -o "$WAN_IF" -j ACCEPT
iptables -A FORWARD -i "$WAN_IF" -o "$LAN_IF" -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT

echo "NAT router started: WAN=$WAN_IF LAN=$LAN_IF EXT_IP=${EXT_IP:-masquerade}"
sleep infinity
