# host-runner

Host-side Linux binary that brings up the eth-examples task set over a Linux
TAP device, so the same stratum client, CPU miner, NTP sync, BTC price
fetcher, metrics exporter, and HTTP server that run on the RP2350 can be
exercised on a laptop or dev box.

Under the hood it reuses the embassy-net stack. By default the link-layer
driver is `embassy-net-tuntap`; setting `HOST_NET_AFPACKET=<ifname>`
switches to the in-tree `embassy-net-afpacket` raw-socket driver bound to
a real NIC (see "Run on a real NIC" below). The firmware and host binary
share `ubos-backend` and the `eth-examples` library; the only
host-specific pieces are this crate, its driver crate, and the network
setup.

## Setup

Run once per boot. The script creates `tap0`, assigns the host-side IP,
and installs the NAT + forward rules needed for outbound traffic from the
embedded stack to reach the internet. Re-execs itself with `sudo` as
needed.

```sh
./setup-network.sh up     # create tap0, enable forward + NAT
./setup-network.sh down   # remove rules, delete tap0
```

Env overrides: `TAP_NAME`, `HOST_PREFIX`, `HOST_SUBNET`.

### What the script does

| Step | Command |
| ---- | ------- |
| Create persistent TAP owned by the current user | `ip tuntap add dev tap0 mode tap user $USER` |
| Bring the link up | `ip link set tap0 up` |
| Assign host-side IP | `ip addr add 192.168.69.1/24 dev tap0` |
| Enable forwarding | `sysctl -w net.ipv4.ip_forward=1` |
| NAT out of the subnet | `iptables -t nat -A POSTROUTING -s 192.168.69.0/24 -j MASQUERADE` |
| Allow forward (src) | `iptables -I FORWARD -s 192.168.69.0/24 -j ACCEPT` |
| Allow forward (conntrack) | `iptables -I FORWARD -d 192.168.69.0/24 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT` |

The scoped FORWARD rules leave `-P FORWARD DROP` in place; only traffic
on the TAP subnet is accepted.

## Run

```sh
cargo run -p host-runner
```

Default filter is `info`. Bump detail with e.g. `RUST_LOG=debug,smoltcp=info`.

### Run on a real NIC (AF_PACKET)

For exercising the runner against a real Ethernet segment (e.g. a
hashboard plugged directly into a dedicated USB-Ethernet adapter), set
`HOST_NET_AFPACKET` to the interface name. The TAP path and its
`setup-network.sh` are skipped entirely.

```sh
sudo -E HOST_NET_AFPACKET=eth1 cargo run -p host-runner
```

Requires `CAP_NET_RAW` (hence `sudo`, or `setcap cap_net_raw=ep` on the
binary). The driver reads the interface MAC and MTU at startup and
reports both to embassy-net.

The AF_PACKET path runs DHCP, so the runner picks up its IP, gateway,
and DNS from whatever DHCP server is on the LAN. Lease and DHCP
transactions are bound to the interface's real MAC. The TAP path keeps
its hardcoded `192.168.69.2/24`.

Caveat: the host kernel keeps processing ingress traffic on the same
interface. To avoid ARP/IP collisions with embassy-net's stack, use a
NIC whose kernel-side IPv4 is unconfigured:

```sh
sudo ip addr flush dev eth1
sudo ip link set eth1 up
```

A veth pair in another namespace works too if you cannot dedicate a
physical NIC.

## Verify

Ping the embedded stack directly:

```sh
ping 192.168.69.2
```

HTML status page:

```sh
curl http://192.168.69.2/
```

Prometheus metrics:

```sh
curl http://192.168.69.2:8081/metrics
```

The `board_ntp_synced`, `board_btc_price_usd`, `board_hashrate_hz`, and
other gauges confirm each task is running end-to-end.
