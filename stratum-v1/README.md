# stratum-v1

Stratum V1 (JSON-RPC over TCP) wire layer plus a `MiningClient` impl
for `stratum-client`. Owns line framing, parsing, message encoders,
target-from-diff conversion, and the per-session subscribe/authorize
state machine.

`no_std`, no `alloc`. The crate borrows `&'static str` for the user /
password fields, so config can live in a `static` next to the firmware's
`AsicSession`.

## Usage

Hand a `StratumV1Client` to `stratum_client::run_stratum_client`:

```rust
static SV1_CONFIG: Sv1Config = Sv1Config { user: "worker.x", password: "" };

let mut client = StratumV1Client::new(&SV1_CONFIG);
run_stratum_client(&mut client, &mut socket, &ASIC_SESSION, &mut state).await
```

The crate is normally not used standalone; firmwares pull it in via
the `backend-uart` or `backend-cpu` feature (both compose
`stratum-v1` + `stratum-client`).

A self-contained host-side smoke test lives in `examples/client_probe.rs`.

## See also

- `stratum-client` — the loop that owns the connection
- `stratum-v2` — sibling adapter for the V2 protocol
