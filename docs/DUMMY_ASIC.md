# Dummy ASIC

The dummy ASIC is a Seeed XIAO ESP32-S3 that pretends to be a real ASIC
over UART: it receives mining Work frames, scans nonces with the ESP32-S3
hardware SHA accelerator, and returns Share frames.

Firmware location: `dummy-asic/`.

## Runtime model

`esp_rtos 0.2.0` runs embassy on both ESP32-S3 cores:

- Core 0 (`PRO_CPU`): UART reader + UART writer tasks.
- Core 1 (`APP_CPU`): nonce solver.

The UART is split into `UartRx` / `UartTx` halves so reader and writer own
independent ends and never contend. `esp_rtos::start_second_core` starts the
`APP_CPU` scheduler; the thread-mode `esp_rtos::embassy::Executor` then runs
on the second core from inside the closure passed to `start_second_core`.

## Physical link

- UART1, 921 600 baud
- XIAO GPIO8 = TX, GPIO9 = RX (wiring table in [SETUP.md](SETUP.md))

## Framing

- Start byte: `0xA5`
- Checksum: XOR-8 over `opcode + payload` (excludes the start byte)

The host-side codec lives in `asic-uart-peer/src/wire.rs`; the dummy-asic
codec lives in `dummy-asic/src/protocol.rs`. Opcodes, frame lengths, and
baud rate must move in lockstep across the two files or the link desyncs
at boot.

## Work frame (host to dummy-asic, opcode `0x01`) -- 119 bytes total

| Offset | Size | Content |
|--------|------|---------|
| 0 | 1 | Start byte `0xA5` |
| 1 | 1 | Opcode `0x01` |
| 2 | 80 | 80-byte Bitcoin block header; solver overwrites the nonce field |
| 82 | 32 | 256-bit target, little-endian; hash accepted when `digest <= target` |
| 114 | 4 | Host-assigned `work_id`, big-endian; echoed in derived shares |
| 118 | 1 | XOR-8 checksum over bytes 1-117 |

## Share frame (dummy-asic to host, opcode `0x02`) -- 43 bytes total

| Offset | Size | Content |
|--------|------|---------|
| 0 | 1 | Start byte `0xA5` |
| 1 | 1 | Opcode `0x02` |
| 2 | 4 | Found nonce, big-endian |
| 6 | 4 | `work_id`, big-endian |
| 10 | 32 | SHA-256d hash |
| 42 | 1 | XOR-8 checksum over bytes 1-41 |

## Solver (APP_CPU)

Code: `dummy-asic/src/solver.rs`.

SHA-256d uses the ESP32-S3 SHA accelerator. Software SHA on Xtensa is far
slower than the UART can drain, so the hardware peripheral is the actual
bound this peer is meant to demonstrate.

The hot path bypasses esp-hal. Bytes 0..64 of the 80-byte header are
constant per Work, so they are hashed once into a midstate and the
per-nonce iteration continues from there. esp-hal exposes save/restore via
`ShaDigest`, but `restore` writes 128 bytes to `m_mem` and 64 bytes to
`h_mem` per nonce -- more CPU than the hardware block it would save.
Touching the register block directly cuts that to 8 H writes plus 16 M
writes per nonce, which is what makes the optimisation pay off.

### Sharp edges

- SHA peripheral register addresses are duplicated from the esp32s3 PAC
  at `0x6003_b000`. The accessors on `register_block()` do not always boil
  down to a single volatile store, so the hot loop touches the registers
  via raw pointers. If the PAC ever remaps the SHA peripheral, this file
  silently produces wrong hashes.
- The hot loop never writes the SHA mode register. It assumes
  `Sha::start::<Sha256>()` in `capture_midstate` set MODE to SHA-256 and
  that nothing else has reset it since. That holds because the solver is
  the sole owner of the SHA peripheral.
- Message and digest words are written in native endianness. The Xtensa
  LX7 in the ESP32-S3 is little-endian, and esp-hal's
  `SocDependentEndianess` resolves to native on non-original ESP32 parts,
  so this matches what the HAL verifier on the host expects. The original
  ESP32 uses a big-endian Tensilica core and would need the opposite
  layout.
- The peripheral's H register is overwritten every iteration: `CONTINUE`
  for the first SHA pulls H from the midstate write, `START` for the
  second SHA resets H to the SHA-256 IV. Nothing else gets to touch H
  between those two points.

## Flashing

esp-hal 1.0.0 uses the ESP-IDF bootloader format. `probe-rs` cannot write
the bootloader on its own, so the first flash must go through `espflash`:

```sh
nix-shell -p espflash --run \
  "espflash flash --chip esp32s3 --port /dev/ttyACM<N> target/xtensa-esp32s3-none-elf/release/dummy-asic"
```

Put the XIAO into download mode first (hold BOOT, press RST). After
flashing, do a full power cycle: disconnect every power source, not just
USB, because the XIAO can be back-powered through the UART wiring.

Subsequent flashes go through probe-rs:

```sh
nix develop .#esp
cd dummy-asic && cargo run --release
```

When multiple ESP-JTAG probes are connected, use the per-main-board
aliases:

```sh
cd dummy-asic && cargo run-rp2040    --release
cd dummy-asic && cargo run-rp2350    --release
cd dummy-asic && cargo run-stm32f407 --release
cd dummy-asic && cargo run-stm32f439 --release
cd dummy-asic && cargo run-esp32s3   --release
```

See `dummy-asic/.cargo/config.toml` for the alias list and probe serials.
