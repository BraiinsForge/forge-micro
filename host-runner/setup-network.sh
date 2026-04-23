#!/usr/bin/env bash
# One-shot TAP + NAT setup for host-runner.
#
# Usage:
#   ./setup-network.sh up     create tap, assign IP, enable forward + NAT
#   ./setup-network.sh down   remove rules, delete tap
#
# Override with env vars: TAP_NAME, HOST_PREFIX, HOST_SUBNET.
#
# Rules are idempotent: re-running `up` does not add duplicates. The script
# re-execs itself with sudo when not run as root.

set -euo pipefail

TAP="${TAP_NAME:-tap0}"
SUBNET="${HOST_SUBNET:-192.168.69.0/24}"
HOST_PREFIX="${HOST_PREFIX:-192.168.69.1/24}"
# Owning user for the persistent TAP. When sudo re-execs us, $USER becomes
# root, so prefer $SUDO_USER when available.
OWNER="${SUDO_USER:-${USER}}"

usage() {
    cat <<EOF
Usage: $0 up|down

  up    create $TAP (owned by $OWNER), assign $HOST_PREFIX,
        enable ip_forward, add NAT masquerade and scoped FORWARD rules
        for $SUBNET
  down  remove the NAT and FORWARD rules and delete $TAP

Environment:
  TAP_NAME     tap device name       (default: tap0)
  HOST_PREFIX  CIDR on the host side (default: 192.168.69.1/24)
  HOST_SUBNET  subnet for NAT match  (default: 192.168.69.0/24)
EOF
}

case "${1:-}" in
    up|down) ;;
    *) usage; exit 1 ;;
esac

if [ "$(id -u)" -ne 0 ]; then
    exec sudo -E "$0" "$@"
fi

cmd_up() {
    if ip link show "$TAP" >/dev/null 2>&1; then
        echo "skipping create: $TAP already exists"
    else
        ip tuntap add dev "$TAP" mode tap user "$OWNER"
        echo "created $TAP (owner $OWNER)"
    fi

    ip link set "$TAP" up

    if ip -o addr show dev "$TAP" | grep -q "$HOST_PREFIX"; then
        echo "skipping addr: $HOST_PREFIX already on $TAP"
    else
        ip addr add "$HOST_PREFIX" dev "$TAP"
        echo "assigned $HOST_PREFIX to $TAP"
    fi

    if [ "$(cat /proc/sys/net/ipv4/ip_forward)" != "1" ]; then
        sysctl -w net.ipv4.ip_forward=1 >/dev/null
        echo "enabled net.ipv4.ip_forward"
    fi

    # NAT masquerade for outbound traffic from the TAP subnet.
    if ! iptables -t nat -C POSTROUTING -s "$SUBNET" -j MASQUERADE 2>/dev/null; then
        iptables -t nat -A POSTROUTING -s "$SUBNET" -j MASQUERADE
        echo "added POSTROUTING masquerade for $SUBNET"
    fi

    # Scoped FORWARD rules so the global -P FORWARD DROP stays intact.
    if ! iptables -C FORWARD -s "$SUBNET" -j ACCEPT 2>/dev/null; then
        iptables -I FORWARD -s "$SUBNET" -j ACCEPT
        echo "added FORWARD accept for src $SUBNET"
    fi
    if ! iptables -C FORWARD -d "$SUBNET" -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null; then
        iptables -I FORWARD -d "$SUBNET" -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
        echo "added FORWARD accept for return traffic to $SUBNET"
    fi

    echo "up: $TAP ready, host-runner can now reach the internet"
}

cmd_down() {
    iptables -t nat -D POSTROUTING -s "$SUBNET" -j MASQUERADE 2>/dev/null && \
        echo "removed POSTROUTING masquerade" || true
    iptables -D FORWARD -s "$SUBNET" -j ACCEPT 2>/dev/null && \
        echo "removed FORWARD src rule" || true
    iptables -D FORWARD -d "$SUBNET" -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null && \
        echo "removed FORWARD conntrack rule" || true

    if ip link show "$TAP" >/dev/null 2>&1; then
        ip link set "$TAP" down
        ip tuntap del dev "$TAP" mode tap
        echo "removed $TAP"
    fi
    echo "down complete"
}

case "$1" in
    up)   cmd_up ;;
    down) cmd_down ;;
esac
