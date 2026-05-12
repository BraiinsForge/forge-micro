# stratum-v2

Stratum V2 (Noise NX handshake + binary framing) wire layer plus a
`MiningClient` impl for `stratum-client`. Built on top of the `ii-sv2`
umbrella crate (security, framing, messages, types). Same plug-in
shape as `stratum-v1`: connect, drive, submit.

`no_std`, no `alloc`. The Noise context and RNG are passed in by the
caller so the firmware controls how much memory `secp256k1` is allowed
to preallocate.

## Usage

Build a `Sv2Config` (owns the worker identity) plus a preallocated
`CryptoCtx`, then drive through `stratum_client::run_stratum_client`:

```rust
let sv2 = SV2_CONFIG_CELL.init(Sv2Config::new("worker.x", pool_pubkey)?);
let ctx = SECP_CTX.init(CryptoCtx::preallocated(SECP_BUF.take())?);

let mut client = StratumV2Client::new(sv2, ctx, BoardRng(rng));
run_stratum_client(&mut client, &mut socket, &ASIC_SESSION, &mut state).await
```

The `stratum2` Cargo feature on each firmware composes this crate
on top of `backend-uart` / `backend-cpu`. Protocol selection (V1 vs V2)
is decided at boot from `POOL_PROTOCOL`.

A self-contained host-side smoke test lives in `examples/client_probe.rs`.

## See also

- `stratum-client` — the loop that owns the connection
- `stratum-v1` — sibling adapter for the V1 protocol
- `~/braiins/micro-sv2` — upstream crate stack this adapter wraps
