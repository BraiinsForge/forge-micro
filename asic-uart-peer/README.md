# asic-uart-peer

Chip-side of an `AsicSession` that drives an external dummy-asic peer
board over UART. Encodes each fresh `WorkAssignment` as a Work frame,
races a Share-frame read against the next assignment, and submits the
peer's solutions back into the session.

`no_std`, no `alloc`. Wire protocol is bit-compatible with the
retired `ubos_main::uart_asic` codec (`FRAME_START = 0xA5`, opcodes
`0x01` / `0x02`, XOR-8 checksum, 119 / 43 byte frames) so existing
`dummy-asic` firmware works unchanged at `921_600` baud.

## Usage

Configure the UART at `asic_uart_peer::BAUD_RATE` and spawn
`run_asic_uart`. On boards whose `RX` and `TX` are typed separately
(e.g. stm32 with DMA ring RX), wrap them in `SplitUart` first:

```rust
#[cfg(feature = "backend-uart")]
#[embassy_executor::task]
async fn asic_uart_task(uart: BufferedUart) -> ! {
    asic_uart_peer::run_asic_uart(&ASIC_SESSION, uart).await
}
```

Selected via the firmware's `backend-uart` feature. Mutually exclusive
with `backend-cpu` (which spawns `asic-cpu-mock` instead).

## See also

- `stratum-client` — owns the `AsicSession` this task feeds
- `asic-cpu-mock` — sibling on-MCU miner for boards without a peer
- `dummy-asic` — peer firmware that speaks the other side of this wire
