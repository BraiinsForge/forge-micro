# asic-cpu-mock

Chip-side of an `AsicSession` that hashes on-MCU instead of driving a
real ASIC. Polls the work cell, scans nonces with double-SHA256, yields
periodically so the executor stays responsive, and pushes found
solutions back into the session.

`no_std`, no `alloc`. Pure-software miner — useful for bring-up and
for boards without an attached ASIC.

## Usage

Spawn `run_asic_cpu` against a `&'static AsicSession` on a core that
won't starve embassy-net:

```rust
#[cfg(feature = "backend-cpu")]
#[embassy_executor::task]
async fn asic_cpu_task() -> ! {
    asic_cpu_mock::run_asic_cpu(&ASIC_SESSION).await
}
```

Selected via the firmware's `backend-cpu` feature. Mutually exclusive
with `backend-uart` (which spawns `asic-uart-peer` instead).

## See also

- `stratum-client` — owns the `AsicSession` this task feeds
- `asic-uart-peer` — sibling that drives an external dummy-asic board
