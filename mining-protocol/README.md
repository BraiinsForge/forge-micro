# mining-protocol

Protocol-agnostic contract between Stratum adapters and the integration
layer. Defines the `MiningClient` trait, the borrowed domain types each
adapter produces (`MiningJob`, `Share`, `JobId`, `SessionUpdate`,
`SubmitResult`), and a handful of pure helpers shared by both protocols
(`hashing`, `hex`, `difficulty`, `lazy_merkle`).

`no_std`, no `alloc`. Not consumed directly by firmware; sits between
`stratum-v1` / `stratum-v2` (impls) and `stratum-client` (driver).

## Usage

Adapter crates implement `MiningClient`:

```rust
impl MiningClient for MyClient<'_> {
    type Error = MyError;
    async fn connect(&mut self, socket: &mut impl Socket) -> Result<(), Self::Error> { ... }
    async fn drive<V: MiningClientVisitor>(&mut self, socket: &mut impl Socket, visitor: &mut V)
        -> Result<(), Self::Error> { ... }
    async fn submit(&mut self, share: &Share<'_>, socket: &mut impl Socket)
        -> Result<(), Self::Error> { ... }
}
```

`stratum-client` consumes the trait via `run_stratum_client`; firmwares
never touch this crate directly.

## See also

- `stratum-v1`, `stratum-v2` — adapter implementations
- `stratum-client` — driver that hosts the trait object
