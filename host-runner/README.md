# host-runner

eth-examples are using embassy net stack.
To run them in Linux userspace you need a way to put raw packets onto network.

Two methods are available:

* TAP device - create a `tap0` network interface and setup NAT. This is recommended for you desktop and doesn't require root to run (only to setup).
* bridged networking - raw packets written into your networking interface. This requires root and is useful for miniminer which doesn't have TAP device in kernel.


## TAP device

- compile with tap feature: `cargo build --release --features tuntap`
- requires TAP device driver in kernel (all major distros do)
- there's a script to setup the `tap0` device: `setup_network.sh`
- the default IP is 192.168.69.2

### Setup

Run once per boot. The script creates `tap0`, assigns the host-side IP,
and installs the NAT + forward rules needed for outbound traffic from the
embedded stack to reach the internet. Re-execs itself with `sudo` as
needed.

```sh
./setup-network.sh up     # create tap0 for your $USER, enable forward + NAT
./setup-network.sh down   # remove rules, delete tap0
```

Env overrides: `TAP_NAME`, `HOST_PREFIX`, `HOST_SUBNET`.

```sh
cargo run -p host-runner --features tuntap
```

Default filter is `info`. Bump detail with e.g. `RUST_LOG=debug,smoltcp=info`.

### Verify

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


## Bridged networking

- default
- injects/captures packet right off the network interface
- useful when running on miniminer
- requires `CAP_NET_RAW` (hence `sudo`, or `setcap cap_net_raw=ep` on the binary)
- it will get IP address from DHCP on your network

### Setup

- `AFPACKET_IF=eth4` will change the network interface; default is `eth0`
- `AFPACKET_MAC=12:34:56:78:9a` sets the MAC address; default is random one based on your current MAC address (so that it doesn't change from run to run and always ends up with the same IP address assigned by the DHCP server)

```sh
AFPACKET_IF=eth4 cargo run -p host-runner
```
