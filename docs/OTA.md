# OTA Firmware Update

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

Boards with an OTA backend — the hashnode among them — support over-the-network
firmware updates via the REST API. An image is uploaded in 4 KiB chunks, written
to a staging slot, and activated on the next boot. If the new image does not
confirm itself, the bootloader automatically reverts to the previous image on
the following boot. No debug probe is required for normal updates.

The upload flow, WebUI behaviour, CSRF handling, session lifecycle, and HTTP
diagnostics are board-agnostic and described once below. The flash mechanism is
board-specific and lives behind the `ubos_ota::OtaBackend` trait: the RP boards
use an `embassy-boot` swap bootloader (the sections immediately following), and
`ubos-ota`'s `esp` feature carries a second backend over the ESP-IDF
bootloader's `otadata` two-slot scheme, documented in
[BOARDS.md](BOARDS.md). The board-agnostic `ubos_ota::OtaEngine`
drives either backend and implements `ubos_api::OtaTarget`, so the REST and
WebUI layers never know which board they are talking to.

## Flash layout

The 2 MiB QSPI flash is partitioned as follows (XIP-mapped addresses):

| Region             | Start        | Size    | Purpose |
|--------------------|--------------|---------|---------|
| `BOOTLOADER`       | `0x10000000` | 128 KiB | Bootloader binary; the RP2350 bootrom runs it on every reset |
| `BOOTLOADER_STATE` | `0x10020000` | 4 KiB   | embassy-boot swap state and progress index |
| `ACTIVE`           | `0x10021000` | 952 KiB | Running application image |
| `DFU`              | `0x1010F000` | 956 KiB | Staging area for the incoming image |
| `CONFIG`           | `0x101FE000` | 8 KiB   | ubos-config persistent store; untouched by embassy-boot |

`DFU` is 4 KiB larger than `ACTIVE` because embassy-boot requires one extra
sector as swap scratch space. `DFU` ends exactly where `CONFIG` begins
(`0x101FE000`), so no flash is wasted. The CONFIG range is defined in
`firmware/hashnode/src/config.rs` as `Flash::CONFIG_RANGE = 0x001FE000..0x00200000`
(offsets from flash base).

## Components

### rp2350-bootloader

`firmware/rp2350-bootloader` is a separate crate that links at `FLASH ORIGIN =
0x10000000`. It carries an RP2350 image-def (`embassy-rp`'s default
`secure_exe`) so the bootrom boots it on every reset — warm or cold — without
consulting the bootrom partition table.

`firmware/rp2040-bootloader` and `firmware/rp2350-bootloader` both call
`rp_bootloader::boot`, the shared entry point in `ubos/rp-bootloader`. Each
crate's `main.rs` only picks the flash size and stays chip-specific; the
`embassy-boot-rp` sequence below is common to both.

The bootloader's startup sequence:

1. Wraps the FLASH peripheral in a `WatchdogFlash` with an 8 s watchdog timeout.
2. Reads partition offsets from the `__bootloader_*` linker symbols baked into
   the binary by `memory.x`.
3. Calls `BootLoader::prepare`, which reads the STATE region and performs any
   pending swap: if STATE holds the swap magic, it copies blocks between `ACTIVE`
   and `DFU`, updating the STATE progress index after each block so a power loss
   can resume. On success STATE is updated to the booted state. An erased STATE
   region (`0xFF` bytes) means plain boot with no swap.
4. Jumps to `ACTIVE` via `bootloader.load(FLASH_BASE + active_offset)`, which
   sets the Cortex-M33 vector table and jumps through the reset vector. Control
   never returns.

The WatchdogFlash arms the hardware watchdog during the swap. If the new image
crashes or hangs before calling `mark_booted`, the watchdog fires, the bootloader
runs again, sees STATE still holding the swap magic (not yet confirmed), and
reverts by performing the inverse swap, restoring the previous image.

### Application

The board-agnostic upload orchestration lives in `ubos/ubos-ota/src/engine.rs`:
`OtaEngine<B>` implements `ubos_api::OtaTarget` and owns the write-offset
tracking, breadcrumb-phase ordering, and core-1 park lifecycle. The RP flash
mechanism is the `OtaBackend` implementation `RpBackend` in
`ubos/ubos-ota/src/rp.rs` (feature `rp`), which wraps an `embassy-boot`
`BlockingFirmwareUpdater` over the DFU and STATE partitions. The partitions are
`BlockingPartition` slices over the single shared flash mutex
(`ubos_ota::SharedFlashMutex`) that the ubos-config store also uses.
`RpOtaTarget` is the type alias `OtaEngine<RpBackend<…>>`.

A board can also implement `ubos_api::OtaTarget` directly, without the
engine. The engine assumes a staging slot and a swap on the next boot; a
board whose stock bootloader mirrors one live slot and verifies it on boot
has neither, so it writes the payload in place and answers the trait
itself. Such a target still shares `ubos_ota::SharedFlashMutex` with the
config store, because that is what keeps two drivers off one chip.

## Upload flow

### Endpoint

`POST /api/system/OTA/chunk` on port 80. The client sends the image one sector
at a time, waiting for a JSON ack before sending the next chunk.

Required headers per request:

- `Content-Length` — byte count of this chunk body (max `OTA_CHUNK_SIZE` = 4096).
- `X-OTA-Offset` — decimal byte offset where this chunk begins in the image.
- `X-OTA-Final` — set to `"1"` on the last chunk only.

The mid-stream ack is `{"ok":true,"next":<next_offset>}`. The final ack is
`{"ok":true}` followed immediately by a board reset; the connection will drop.

The one-chunk-at-a-time protocol exists because the WIZnet Ethernet controllers
have small hardware RX buffers (4 KiB on the W6100). Each flash write pauses
both cores (the embassy-rp flash driver runs erase/program from RAM and gates
XIP), so the chip buffer cannot be drained while the write is in progress.
Sending data faster than one
sector per ack would overflow the chip buffer. The `OTA_DRAIN_DELAY_MS = 5 ms`
pause after each write gives the network task time to drain the buffer, send
ACKs, and reopen the TCP window.

The `POST /api/system/OTA` streaming endpoint also exists. It accepts the entire
image body in one request and writes it to flash sector by sector as the bytes
arrive, without per-chunk acks. It is kept for curl and Basic-auth clients that
prefer a single-shot upload. Streaming self-heals mining on a dropped connection
but is not resumable: a drop loses all progress and the upload restarts from
byte 0. The WebUI firmware-update card uses the chunk-ack endpoint; it no longer
uses the streaming endpoint.

### WebUI client behavior

The WebUI drives uploads through the chunk-ack endpoint. Progress is driven by
the acked `next` offset, so the progress bar reflects bytes actually written to
flash, not bytes sent over the network.

After the final chunk is acked (or the connection drops after the final chunk),
the WebUI treats the upload as complete and shows the full-page
"updating / rebooting" notice, which polls the board until it returns — the same
screen pattern used after a factory reset. Transient errors on any chunk cause
the client to retry that chunk. A `409` response means the session was reset
(for example by the 30 s idle watchdog) and the client restarts from offset 0,
re-triggering `begin()`.

### CSRF for chunked uploads

The chunk endpoint applies CSRF protection selectively based on the chunk
position and authentication method:

- Session-cookie callers (the WebUI) must supply a CSRF token **only on the
  offset-0 chunk** — the request that starts the session, stops mining, cuts the
  PSU rail, and calls `begin()`.
- Continuation chunks (`X-OTA-Offset > 0`) are CSRF-exempt. They cannot start a
  session; they are bound to the single in-progress upload by the target mutex
  (one active session at a time) and the handler's offset-continuity check (a
  `409` is returned on mismatch). An attacker without a valid CSRF token cannot
  initiate an OTA session. A forged continuation could at worst corrupt bytes in
  an already-started upload, which fails image validation and triggers automatic
  rollback — it cannot install firmware.
- Basic-auth requests are fully CSRF-exempt, so curl-driven chunk uploads require
  no token.

The WebUI and the API answer on one port, so a chunk POST is same-origin.
The browser sends no preflight, and the firmware sends no CORS headers.
Each chunk therefore costs one request rather than two.

### Session lifecycle

1. First chunk (`X-OTA-Offset: 0`): the handler sends `OtaBegin` to the manager and calls `mining.wait_stopped`.
   The manager asserts the OTA pause, and the wait ends at the miner's `Stopped` state.
   A miner that never started is already stopped.
   The miner asserts chip reset and attempts to disable the PSU. It logs any PSU-disable error.
   If the wait exceeds five seconds, the handler refuses the update.

   The handler then calls `ota.begin()`.
   The stopped state does not prove that serial workers are safe for flash writes.
   The OTA engine checks this separately through the park mechanism below.

2. `begin()` verifies the running image is in the confirmed booted state, then
   calls `park_core1_and_wait()`. This spin-polls `is_parked` on the
   `ubos_mining::Park` that the board handed to the engine (with a 50 ms
   budget, 100 µs per poll) after repeatedly calling
   `request` on it. A board with more than one chain runs
   one serial actor per chain on core 1, and `is_parked` returns `true`
   only once every live actor has reached its own park point; see [Multiple
   chains and the core-1 park](MINING.md#multiple-chains-and-the-core-1-park).
   The 50 ms budget and the 100 µs poll interval must cover every actor's
   park, not just one, because each actor yields to the executor on its own
   turn rather than parking synchronously with the others. The pre-emptive
   park is necessary because the `portable-atomic` `AtomicU64` seqlock
   fallback disables interrupts during reads; `embassy-rp`'s `pause_core1()`
   uses `SIO_IRQ_FIFO` which is blocked during those windows, causing the
   handshake to stall for over 15 s. With core 1 pre-parked, `pause_core1()`
   finds every actor already idle and returns immediately.
   `begin()` also writes the OTA breadcrumb to watchdog SCRATCH0 (magic
   `0x4F54_5541`), SCRATCH1 (current write offset), and SCRATCH2 (write phase),
   so a crash during the upload is diagnosable at the next boot.

3. Each subsequent chunk is written by `write_chunk`, which calls
   `updater.write_firmware(offset, data)`. The updater erases each 4 KiB DFU
   sector lazily just before its first write. SCRATCH2 is set to `WRITE_PHASE_IN_FLASH`
   before the call and `WRITE_PHASE_FLASH_DONE` after, so a watchdog timeout
   inside the flash op is distinguishable from one between chunks.

4. The final chunk (`X-OTA-Final: 1`) calls `finish()` after the write.
   `finish()` calls `updater.mark_updated()`, which writes the swap magic into
   the STATE partition. It then clears the OTA breadcrumb (SCRATCH0 and SCRATCH2)
   and releases the core-1 park via `release` on that `Park`.

5. If `ota.staged_config()` gives a value other than `None`, the handler
   calls `config.downgrade()` with it. The call waits up to five seconds for the
   config task to write the record for the new image. If the wait exceeds five
   seconds, the handler continues without the record. See
   [The config across an update](#the-config-across-an-update).

   The handler writes the `{"ok":true}` ack and calls `ota.reboot_into_new()`,
   which releases the core-1 park and calls `SCB::sys_reset()`. The ack write is
   attempted but its result is discarded: the reboot must happen regardless of
   whether the TCP write succeeds, because leaving mining stopped and the swap
   pending would strand the board.

On any error after `begin()` succeeds, the handler calls `ota.abort()`, which
releases the core-1 park, clears the breadcrumb, and resets `write_offset` to
`None`. Mining is then restarted via `manager.send(ManagerMessage::OtaEnd)`.

If the client disappears mid-session no error reaches any handler, so the
per-handler restart never runs. A background watchdog reclaims the session:
`ota_session_watchdog` wakes every 30 s (`OTA_SESSION_IDLE_TIMEOUT`) and, when
the phase is still `Uploading` with no new chunks since the previous tick, takes
the target mutex, calls `abort()`, and resumes mining via `OtaEnd`. It **aborts**
rather than merely resuming. If mining restarted while the target stayed armed
at offset `N`, a returning stale client sending offset `N` would pass the offset
check and write flash while the core-1 mining worker runs — the exact race
`begin()` guards against. `abort()` resets `write_offset` to `None`, so the
stale client fails the offset check, receives a `409`, and is forced back
through offset 0, which re-stops mining via `begin()`.

## Boot and swap

On the reset that follows a completed upload:

1. The bootloader runs. It reads STATE, finds the swap magic, and performs the
   block-by-block swap of `ACTIVE` and `DFU`, writing progress into STATE after
   each block.
2. The bootloader jumps to `ACTIVE`. The new image starts executing.
3. Early in `boot()`, before core 1 starts, the firmware calls
   `ota_target::mark_booted(shared_flash)`. This calls `updater.mark_booted()`,
   which writes the confirmed boot state into STATE and clears the swap magic.
   The firmware logs `"boot: firmware v{}"` with `config::FIRMWARE_VERSION`
   immediately before this call so the version change is visible on the defmt
   console.

After `mark_booted` succeeds, the bootloader will not attempt a revert on the
next reset.

## Automatic rollback

If the new image does not call `mark_booted` before the bootloader's 8 s
watchdog fires — because it crashed, panicked, or entered an infinite loop —
the board resets. The bootloader runs again, reads STATE, sees the swap magic
still present (the image never confirmed), and reverses the swap by swapping
`ACTIVE` and `DFU` back. The previously running image is restored to `ACTIVE`
and boots normally; it calls `mark_booted` and confirms itself, leaving the
system stable.

This was validated on hardware on 2026-06-18: the full cycle v1 → v2 →
deliberately broken v3 (hung before `mark_booted`) → auto-revert to v2 → v4
passed end to end.

The application's cooperative watchdog has a 2 s timeout (`Watchdog::TIMEOUT`)
and is fed unconditionally by the watchdog task. Both the bootloader's 8 s
window and the application's 2 s window are relevant: the bootloader's window
covers the startup path to `mark_booted`; once the application is running and
confirmed, the application's watchdog guards normal operation.

## No manual revert

The swap is not its own inverse. After a forward swap the first page of the DFU
slot holds the newly-installed image's vector table, not the previous image's.
Triggering a second swap without writing a fresh image would produce a corrupt
ACTIVE (mismatched vector table and body). Therefore there is no
`/api/system/OTA/revert` endpoint. The `POST /api/system/OTA/revert` endpoint
and `OtaTarget::reboot_into_previous` that appeared in an earlier build have
been removed.

To return to an older version, re-upload it through the normal OTA path. The
automatic rollback (above) is the real safety net: it reverses a swap that was
never confirmed while the undo data is still intact.

The `/api/info` `ota` object includes `hasPreviousImage` and `previousVersion`
as informational fields only. `hasPreviousImage` inspects the DFU slot's first
vector-table words to determine whether a plausible image is staged there.
`previousVersion` is always `null` because embassy-boot stores no per-image
version metadata.

## The config across an update

The firmware stores the config as one record. The record starts with the
schema hash of the firmware that wrote it. An update can change the schema.

Each change to the schema adds a version. The list of versions and the steps
between their schemas are in `ubos/ubos-config/src/versions/mod.rs`. The
newest version in the list must have the schema of the firmware. If it does
not, the build stops with an error.

On boot, the firmware reads a record in its own schema directly. It upgrades a
record in the schema of an older version in the list. If the record has
another schema, the firmware starts from the defaults.

The firmware cannot read the record of a version newer than its own. Before
the reboot into the new image, both OTA handlers tell the config task which
schema the new image reads. The task then writes at most one record, and no
other record until the reboot.

If the schema belongs to an older version in the list, the task writes the
record in that schema. If not, the task writes no record. An image with the
same schema reads the stored record, and a newer image upgrades it. Any other
image starts from the defaults.

The OTA target gives that schema through `OtaTarget::staged_config`. If it
gives `None`, the handler tells the config task nothing. The RP and ESP boards
give `None`, because their images declare no schema. On these boards, an OTA
upgrade keeps the config, and an OTA downgrade to an older schema loses it.

A target can also report that the new image is the firmware that the board
replaced. The task then writes the record of the oldest version in the list.
If the operator installs a ubos image after it, that image upgrades from that
record.

## Diagnostics over HTTP

`GET /api/info` includes an `ota` object when an OTA target is
registered. All fields are read lock-free from published atomics; the info
endpoint never touches the upload mutex and stays readable during an active
upload.

Fields emitted by `GET /api/info`:

| Field | Type | Description |
|-------|------|-------------|
| `runningSlot` | string | Always `"ACTIVE"` (swap-based, no A/B slot identity) |
| `runningVersion` | number | Version compiled into the running image via `UBOS_FW_VERSION` |
| `hasPreviousImage` | bool | Whether a plausible image exists in the DFU slot |
| `previousVersion` | number\|null | Always `null` (embassy-boot stores no version) |
| `phase` | string | Current upload phase: `"idle"`, `"uploading"`, `"error"`, or `"rebooting"` |
| `bytesWritten` | number | Bytes written in the current or last upload session |
| `chunks` | number | Number of chunks written in the current or last session |
| `lastError` | number | Error code from the last failed session (0 = none; 1 = NoPartitionTable, 2 = SlotOverflow, 3 = Flash, 4 = ShortUpload, 5 = WrongHardware, 6 = Connection, 7 = InvalidImage) |
| `resetReason` | string | Cause of the most recent reset: `"clean"`, `"watchdogTimeout"`, `"watchdogForced"`, or `"unknown"` |
| `resetOtaActive` | bool | Whether an OTA session breadcrumb (SCRATCH0 = `0x4F54_5541`) was present at boot |
| `resetOtaOffset` | number | Byte offset recorded in SCRATCH1 at the time of reset (0 when no session was active) |
| `resetWritePhase` | string | Write phase in SCRATCH2 at boot: `"none"`, `"inFlash"`, `"flashDone"`, or `"panic"` |

The watchdog scratch registers (SCRATCH0–SCRATCH2) survive a warm reset but are
cleared by a power-on reset. `begin()` writes the magic and initial phase;
`write_chunk()` updates SCRATCH1 (offset) and SCRATCH2 (phase) around each
flash call; `finish()` clears SCRATCH0 and SCRATCH2. The panic handler
(`ota_target::record_panic_breadcrumb`) writes `WRITE_PHASE_PANIC` (3) into
SCRATCH2 from any panic, not only during an OTA session. At boot, `main.rs`
reads and publishes all four values, then clears SCRATCH0 and SCRATCH2, so the
diagnostics survive exactly one boot cycle. SCRATCH1 (offset) is left untouched;
it is only meaningful while SCRATCH0 holds the magic, so a stale offset is
ignored once SCRATCH0 is cleared.

## Build and dev loop

The application crate (`firmware/hashnode`) has a single build variant.
`memory.x` sets `FLASH ORIGIN = 0x10021000`, which is the XIP address of the
`ACTIVE` region. The same binary is used for both probe-rs flashing and OTA
delivery.

Version stamping: the build reads `UBOS_FW_VERSION` from the environment at
compile time. When the variable is absent, `FIRMWARE_VERSION` defaults to `1`.
The parse is done at compile time by a `const fn` so the value is baked into
the binary.

Slot-size gate: the flake's RP board build derivations, `build-hashnode`
among them, each run `arm-none-eabi-objcopy -O binary` on the release ELF and
reject images larger than `slotMaxBytes = 952 * 1024` (the ACTIVE slot size).

Initial dev loop for a board that has never had the bootloader flashed:

```sh
# Flash the bootloader once.
probe-rs erase --chip RP235x
probe-rs download --chip RP235x firmware/rp2350-bootloader/target/.../rp2350-bootloader.bin --base-address 0x10000000

# Thereafter, flash the application at the ACTIVE offset.
nix develop .#arm -c bash -c 'cd firmware/hashnode && cargo run --release'
```

`cargo run` flashes at the ACTIVE offset (`0x10021000`). A warm reset from
probe-rs triggers the bootloader, which reads STATE (`0xFF` = plain boot) and
chains directly to `ACTIVE`. STATE must hold the confirmed boot magic before an
OTA upload is accepted; `mark_booted` writes this on the first run after
flashing.

## Limits and not implemented

- **No image signing.** Images are not signed or authenticated. Any client with
  network access and valid credentials can install any binary.
- **No anti-rollback.** There is no monotonic version counter; older images can
  be installed freely.
- **Hardware validation is per board.** The full upload → swap → confirm →
  rollback cycle has been validated end to end on one RP2350-class board;
  the hashnode itself has not yet been exercised through the full cycle.
  Per-board validation status is tracked in [BOARDS.md](BOARDS.md).
