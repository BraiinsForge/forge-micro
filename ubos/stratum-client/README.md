# stratum-client

Integration loop and shared session state that drives a pool connection
to completion regardless of protocol version. Owns the read/submit/
keepalive arbitration, ntime-rolling work generator, and the
`AsicSession` (`Watch<WorkAssignment>` + `Channel<Solution>`) that the
chip-side task talks to.

`no_std`, no `alloc`. The protocol adapter is plugged in as a
`MiningClient` impl; the chip-side is anything that drives the
`AsicSession`.

## Usage

Firmware boot wires three statics and spawns one task per side:

```rust
static ASIC_SESSION: AsicSession = AsicSession::new();
static SV1_CONFIG: stratum_v1::Sv1Config = stratum_v1::Sv1Config { user, password };

async fn stratum_task(stack: Stack<'static>) -> ! {
    let mut client = stratum_v1::StratumV1Client::new(&SV1_CONFIG);
    let mut state = StratumState::new();
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.connect(pool_addr).await.unwrap();
    run_stratum_client(&mut client, &mut socket, &ASIC_SESSION, &mut state).await
}
```

The chip side reads `ASIC_SESSION.work_receiver()` and pushes solutions
via `ASIC_SESSION.submit_solution(...)`. The firmware drives the BM1362
hashboard via `ubos-miner`; `host-runner` contains an inline CPU mock
used as a fallback when no hashboard is present.

## See also

- `mining-protocol` — `MiningClient` trait the adapter implements
- `ubos-miner` — BM1362 driver that consumes `AsicSession` on real hardware
