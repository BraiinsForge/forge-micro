# host-runner

ubos-main are using embassy net stack. To run them in Linux userspace
you need a way to put raw packets onto the network. The transport is
selected by target architecture:

* `x86_64` (dev host) — TAP device, set up via `setup-network.sh`. No
  root at runtime, default IP `192.168.69.2`.
* `arm` (miniminer) — bridged networking via `AF_PACKET`. Requires
  `CAP_NET_RAW` (run via the `runner.py` cargo runner, which `ssh`s as
  root). Gets its IP from the LAN DHCP server.


## Dev shells

* `nix develop .#host` — native toolchain. Plain `cargo run -p host-runner`
  builds and runs the TAP variant on x86_64.
* `nix develop .#bmm` — cross-toolchain for the miniminer (BMM). Sets
  `CARGO_BUILD_TARGET=arm-unknown-linux-musleabi`, the matching linker,
  and `CARGO_TARGET_..._RUNNER` pointing at `scripts/runner.py`. Plain
  `cargo run -p host-runner` cross-builds, ships, and runs on the target.


## Host (TAP)

`setup-network.sh` runs once per boot. It creates `tap0`, assigns the
host-side IP, and installs the NAT + forward rules needed for outbound
traffic from the embedded stack to reach the internet. Re-execs itself
with `sudo` as needed.

```sh
./setup-network.sh up     # create tap0 for your $USER, enable forward + NAT
./setup-network.sh down   # remove rules, delete tap0
```

Env overrides: `TAP_NAME`, `HOST_PREFIX`, `HOST_SUBNET`.

```sh
nix develop .#host -c cargo run -p host-runner
```

Default log filter is `info`. Bump with e.g. `RUST_LOG=debug,smoltcp=info`.

### Pool configuration

Same `POOL_*` env-var contract as the firmware; see `../docs/SETUP.md`
for the full table.

Stratum V1 against Braiins pool:

```sh
POOL_HOST=172.65.65.63 \
POOL_PORT=3333 \
POOL_PROTOCOL=sv1 \
POOL_USER=<braiins-account>.<worker> \
POOL_SV1_PASS=x \
nix develop .#host -c cargo run -p host-runner
```

### Verify

```sh
ping 192.168.69.2
curl http://192.168.69.2/
curl http://192.168.69.2:8081/metrics
```


## BMM (miniminer, AF_PACKET)

Set the target host (and optionally user/password) via env or via
`scripts/runner.py` flags. The rest of the runner's knobs are flag-only;
see `scripts/runner.py --help`.

| Env var        | Flag         | Default |
|----------------|--------------|---------|
| `BMM_HOST`     | `--hostname` | required |
| `BMM_USER`     | `--user`     | `root` |
| `BMM_PASSWORD` | `--password` | (key auth) |

```sh
BMM_HOST=10.x.x.x nix develop .#bmm -c cargo run -p host-runner
BMM_HOST=10.x.x.x nix develop .#bmm -c env RUST_LOG=debug cargo run -p host-runner
```

`RUST_LOG`, `RUST_BACKTRACE`, and any `INJECT_*` env vars are propagated
to the remote process. The runner script is vendored from bos-main with
beaglebone, bosminer test prep, tmux gateway, and similar branches
trimmed; see `scripts/runner.py`.

`AFPACKET_IF` (default `eth0`) selects the interface; `AFPACKET_MAC` sets
a fixed MAC. Without `AFPACKET_MAC`, a stable synthetic MAC is derived
from the host NIC's real MAC so DHCP leases persist across runs.

Any local `host-runner/.cargo/config` from older workflows is now
obsolete and can be deleted; the `bmm` shell sets the linker via env var.
