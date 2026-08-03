# Setup

## Udev rules

Debug probes need udev rules for non-root access. Copy the bundled rules
file and reload:

```sh
sudo cp ../assets/69-probe-rs.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

On NixOS, use `services.udev.extraRules` instead of copying files. The
rules file covers ST-Link, SEGGER J-Link, CMSIS-DAP, Espressif, and FTDI
probes.

## ST-Link V3 firmware

`probe-rs` requires ST-Link firmware V3J3 or newer. Check with:

```sh
nix shell nixpkgs#stlink -c st-info --probe
```

If the J-version is too low, update with ST's updater (STSW-LINK007). The
updater is a Java application and needs `libusb-1.0`:

```sh
nix shell nixpkgs#jdk nixpkgs#libusb1 -c bash -c \
  'LD_LIBRARY_PATH=$(nix eval --raw nixpkgs#libusb1)/lib \
   java -Djava.library.path=<path>/native/linux_x64 \
   -jar <path>/STLinkUpgrade.jar'
```

## Debug probe (Pico 2 as CMSIS-DAP)

A Raspberry Pi Pico 2 running the debugprobe firmware provides SWD and
UART passthrough. Download the Pico 2 UF2 from the upstream
[raspberrypi/debugprobe releases](https://github.com/raspberrypi/debugprobe/releases)
and flash it via BOOTSEL mass storage.

On the probe, GP2 is the SWD clock, GP3 the SWD data, GP4/GP5 the UART
passthrough TX/RX pair, and GND must be common with the target.

## Pin assignments

### Braiins Forge Miner x4 (a.k.a. Hashnode)

RP2350B with a W5500 over SPI0. Drives a 4-chip BM1370 hashboard via
UART1 and a BfmPsu (SIC45x) over I2C1. GPIO30 (`SIC_EN`, active-high,
10 kΩ pull-down) gates the SIC45x power stage; it is driven high by the
miner task during bring-up. GPIO23 is the third fan tachometer input
(`RP_TACH_C`). The full peripheral map, including the SPI0 and I2C1
pinout, is in `firmware/hashnode/src/config.rs`.

## Building and flashing

ARM targets share one dev shell:

```sh
nix develop .#arm
cd firmware/hashnode && cargo run --release
```

`cargo run` flashes over a probe attached to this machine. When only
one probe is attached, the runner selects it automatically. Export
`$PROBE_SERIAL` to pick one when several are attached.

Every board drives a BM13xx hashboard via the `ubos-mining` backend
unconditionally. The `display` feature is enabled by default on
boards that have a display.

## Pool configuration

Pool config lives in the `ubos-config` record store and is persisted in
the on-chip flash. The HTTP API mutates it at runtime; the `POOL_*` env
vars only seed `Config::default`, which is the value used on a fresh
board before the first store. A stored blob in flash supersedes both.

Set the env vars before `cargo build` / `cargo run` to change the
factory defaults of a freshly flashed board:

```sh
POOL_HOST=stratum.example.com POOL_PORT=3333 cargo run --release
```

| Variable | Type | Default | Notes |
|---|---|---|---|
| `POOL_HOST` | string | `stratum.braiins.com` | IPv4 literal or DNS name |
| `POOL_PORT` | u16 | `3333` | |
| `POOL_USER` | string | empty | worker identity, P2TR address + `.workersuffix` |
| `POOL_PASSWORD` | string | unset → no password | seeds the Sv1 password slot of the default `PoolEntry` |

Code: `ubos/ubos-config/src/lib.rs` (`DEFAULT_POOL_*` and `Config::default`).

## mDNS discovery

Each board advertises itself via three mDNS services so Prometheus,
browsers, REST clients, and ad-hoc tooling can find it without static
config. The announcement carries an `A` record for `<hostname>.local`
and one `SRV`/`TXT`/`PTR` set per service. Each TXT record carries
`path=`, `board=`, and `mcu=`.

| Service              | Port | Purpose                    |
|----------------------|------|----------------------------|
| `_http._tcp.local`   | 80   | Web UI (dashboard/login)   |
| `_ubos._tcp.local`   | 8080 | JSON REST API              |
| `_prometheus._tcp.local` | 8081 | Prometheus scrape target |

Browsing requires `avahi-daemon` on the workstation. On NixOS, enable
it in the host configuration. The `nssmdns4` switch also wires mDNS
into NSS so `ping <hostname>.local` and any other hostname-based
tool resolves through the same announcements.

```nix
services.avahi = {
  enable = true;
  nssmdns4 = true;
};
```

Then browse every advertised board:

```sh
avahi-browse -rt _prometheus._tcp
avahi-browse -rt _http._tcp
avahi-browse -rt _ubos._tcp
```

Without the daemon, a unicast query against a known IP also works
because edge-mdns answers on port 5353:

```sh
dig +time=3 +tries=1 -p 5353 @<board-ip> _prometheus._tcp.local PTR
dig +time=3 +tries=1 -p 5353 @<board-ip> _http._tcp.local PTR
dig +time=3 +tries=1 -p 5353 @<board-ip> _ubos._tcp.local PTR
```
