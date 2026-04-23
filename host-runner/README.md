# host-runner

Host-side Linux binary that brings up the eth-examples task set over a Linux
TAP device, so the same stratum client, CPU miner, NTP sync, BTC price
fetcher, metrics exporter, and HTTP server that run on the RP2350 can be
exercised on a laptop or dev box.

Under the hood it reuses the embassy-net stack with `embassy-net-tuntap`
as the link-layer driver. The firmware and host binary share
`ubos-backend` and the `eth-examples` library; the only host-specific
pieces are this crate and its network setup.

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
