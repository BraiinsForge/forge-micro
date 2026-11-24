# The fee slot

> **Status.** Implemented under UBOS-165. Kept aligned with the code in
> `ubos/ubos-config`, `ubos/ubos-mining` and `ubos/ubos-firmware` —
> when the two disagree, the code wins and this file needs a patch.

A board can run one pool slot the operator cannot edit: the fee pool,
at a fixed share of the delivered work. One build-time switch decides
whether an image ships with it. This page tells you how to build a
firmware with the slot and how to read it from the API, the metrics
and the logs. The design section says why it has the shape it has.
The shape inside the pool stack is the fee slot section of
`docs/MULTIPOOL_DESIGN.md`.

The boards of this tree mine the Braiins fee server. Its host, port,
key, share and worker name live once, as
`ubos_config::fee::BRAIINS_POOL`. A product outside the tree states
its own server, key and share the same way, in the firmware that runs
them, with a worker of its own that builds the name the pool reads.
The fee takes a board's last session slot, after its operator slots.

## The switch

`DEVFEE=1` in the environment of the `cargo` command ships the fee
slot. `DEVFEE=0` or an unset `DEVFEE` ships none. Any other value
stops the build with `DEVFEE must be 0, 1 or unset`.

Every board crate states the switch in its own `.cargo/config.toml`:

```toml
[env]
DEVFEE = "0"
```

Cargo reads that file from the crate's directory, so a hand build in
the crate gets the state the line says. Every board in this tree
states `0`, so every image here ships no fee. A product in a tree that
consumes this one states `1` in the same place. A crate that states
no line builds as if it stated `0`. The boards write the line anyway,
so a reader of the config sees the state instead of inferring it.

A value on the `cargo` command beats the crate config. `DEVFEE=1
cargo build` turns one hand build of a board here on, and `DEVFEE=0
cargo build` turns one build of a product off. Nothing can unset a
value the config adds, which is why `0` exists.

An off build has no fee row in the API, no fee session and no fee
series in the metrics. Every session slot of the board is an operator
slot, as before the switch existed. The board source is the same in
both states: a board that ships the slot hands the config task a fee
slot and names the fee scheduler, and the switch decides what they
do.

A nix build never reads a cargo config, because crane runs cargo
from the flake root. So `nix/fee.nix` reads the line for the gates and
the images, and an image and the gates of its board build the state
the crate states. The host gates run with the switch on, and
`test-ubos-config-off` runs the config crate without it, so both
states are gated. The on state of the board crates is gated by a
`clippy-<board>-on` and a `build-<board>-on` row each for `rp2040`,
`rp2350` and `esp32s3`, one board per target that can carry the fee,
so the path no image here builds cannot rot unseen and the RAM a
fourth session costs is measured. A dev shell exports nothing: the
crate config is the one place the state is written.

## Which slot runs the fee

A board states how many stratum sessions it affords as `POOL_SLOTS`.
With the switch on, the last of them is the fee slot and the others
are the operator's. With the switch off, all of them are the
operator's. The fee needs an operator slot beside it, so a board with
one session runs no fee, and a build that places one there stops
with `a fee slot needs an operator slot beside it`.

| Board | Sessions | Switch off | Switch on |
|---|---|---|---|
| `bmm-pico`, `forge-axe`, `hashnode`, `rp2350` | 4 | operator 0, 1, 2 | operator 0, 1, 2 and the fee at 3 |
| `rp2040`, `esp32s3` | 3 | operator 0, 1, 2 | operator 0, 1 and the fee at 2 |

The stored config keeps three operator entries on every board. A
board with two operator slots runs the first two and keeps the third,
as `rp2040` and `esp32s3` do with the switch on. The fee costs such a
board no RAM: the session count does not change.

## Build

A board of this tree applies the build's overrides to the shared pool
in its `config.rs`:

```rust
pub const FEE_POOL: ubos_config::FeePool = ubos_config::fee::BRAIINS_POOL.with_overrides();
```

A product outside the tree states every field, with a worker of its
own:

```rust
pub const FEE_POOL: ubos_config::FeePool = ubos_config::FeePool {
    host: "fee.example.com",
    port: 4444,
    pubkey: Some("<base58check>"),
    worker: &PRODUCT_WORKER,
    permille: 20,
}
.with_overrides();
```

Then `main` places it at the board's last session slot and hands it
to the config task and the serial task:

```rust
pub(crate) const FEE_SLOT: Option<ubos_config::FeeSlot> =
    ubos_config::FeeSlot::last_of(config::FEE_POOL, POOL_SLOTS);

ubos_config::config_task!(BoardConfigStorage, &config::BOARD.defaults, &config::MINING_INFO, FEE_SLOT);
```

```rust
scheduler = ubos_mining::scheduler::FeeScheduler::new(config::FEE_POOL.permille)
```

`FEE_SLOT` is `None` on an off build, so the lines read the same in
both states. The config task keeps the slot, and the readers of its
entry ask the config handle for it: `pool_slot(slot)` builds the
entry a session runs, and `view()` serves it on the API.

The `DEVFEE_*` variables override the board's values at build time.
An unset variable keeps the board's value.

| Variable | Meaning |
|---|---|
| `DEVFEE_POOL_HOST` | host of the fee pool |
| `DEVFEE_POOL_PORT` | TCP port, 1 to 65535 |
| `DEVFEE_POOL_USER` | the whole worker name, in place of the worker's |
| `DEVFEE_POOL_PUBKEY` | SV2 authority key, base58check |
| `DEVFEE_PERMILLE` | share of the work, in thousandths, 0 to 499 |

An authority key belongs to one operator's servers. A build that
overrides the host and not the key therefore runs without a signature
check. A build that overrides both runs with the key it names. A key
without a host stops the build with `DEVFEE_POOL_PUBKEY needs
DEVFEE_POOL_HOST`, because the board's server does not answer to a
foreign key. A host override keeps the board's port, so a server on
another port takes `DEVFEE_POOL_PORT` as well. The entry is SV2, so an
SV1 pool cannot serve the slot.

Every override is checked when the config crate compiles, so a wrong
one stops the build. A share that is not a number from 0 to 499 stops
it with `DEVFEE_PERMILLE must be a number from 0 to 499`. The bound
is the largest share the scheduler can repay after an outage, which
`MAX_FEE_PERMILLE` in the config crate states. A port outside 1 to
65535 stops it with the same kind of message, and so does a host or a
worker name past 128 bytes. A key that is not
51 characters of the base58 alphabet stops it with
`DEVFEE_POOL_PUBKEY must be 51 base58 characters`. A key of the right
shape and a wrong checksum passes the build and stops the first boot
of that image with a `BUG:` panic.

Example, from the crate's own directory in its shell:

```sh
DEVFEE=1 DEVFEE_POOL_HOST=pool.example.com DEVFEE_POOL_PORT=3333 \
DEVFEE_POOL_USER=fee.worker DEVFEE_PERMILLE=20 cargo build --release
```

## The worker name

The fee pool identifies a miner by its worker name. The product that
states the fee pool builds the name, as a `FeeWorker` the pool
carries. The config crate asks it on every read of the fee entry. It
hands the worker the operator's pools as currently configured, so a
name can follow the operator's account. A config change restarts the
fee session, which then authorises with the name the next read gives.

The boards of this tree send one fixed name,
`ubos_config::fee::UNREGISTERED`, which the pool reads as a device it
has not registered. The pool pays such a fee to the fee account as a
whole and credits it to no board. A product outside the tree states a
worker that builds the name the pool registered for it.

`DEVFEE_POOL_USER` replaces the whole name, for a bench build.

## How the share is kept

The scheduler counts difficulty-1 shares from the chips, not time. It
holds a debt in thousandths of a share. Each operator share adds the
share's numerator to the debt. The fee is due when the debt reaches
the rest of a share, `1000 - permille`. The next work then goes to
the fee pool, and that share subtracts that rest. At 20 per mille
that is one fee share for each 49 operator shares. The split is exact
in the long run and does not depend on the operator weights.

The fee slot is not a backup. When no operator pool has work, the
chain idles, even with a fee share due. When the fee pool is down,
the operator pools get all the work and the debt grows, up to 1000
fee shares. When the fee pool returns, it is repaid up to that cap,
one fee share for every operator share, so the operator keeps half
of the work while the debt is paid and the whole of it after.

## The REST API

`GET /api/config` serves the fee entry as `fee`, beside the
operator's `pool` list, in the same shape as a `pool` entry, and the
share the slot takes as `fee_permille`, in thousandths. A board that
runs no fee slot serves neither key. A `POST /api/config` accepts
both and ignores them. A client that posts the `GET` body back
therefore leaves the list as it was.

```sh
curl -s -u root:root http://<board>/api/config | jq '{pool: [.pool[] | {host, user}], fee: .fee | {host, user}, share: .fee_permille}'
```

`GET /api/info` tells you whether the fee session is up and whether a
chain is mining it:

- `pools` is a bitmap of the session slots with an open connection.
  The fee slot is the board's last: bit 3, the value 8, on a board
  with four sessions, and bit 2, the value 4, on one with three.
- `chains[].pool` is the slot whose work the chain was last given.
  With a 20 per mille share, a chain reads the fee slot on about one
  poll in 50.

```sh
curl -s -u root:root http://<board>/api/info | jq '{pools, mining: [.chains[].pool]}'
```

The web UI shows the fee entry as the last row of the Pools tab,
read-only. The dashboard's three cards read the operator's three
entries by index. On a board with four sessions, the fee slot has no
card. On a board with three sessions, the third card shows the state
of session slot 2, which is the fee's, over the operator's third
entry.

## Prometheus

Every per-pool series carries its slot in the `pool` label. An
operator slot carries its number, and the fee slot carries `fee`,
whichever slot runs it, so a query reads the same on every board.

| Series | Meaning |
|---|---|
| `board_pool_work_shares_total{chain,pool}` | difficulty-1 chip shares, by the slot that got the work |
| `board_shares_accepted_total{pool}` | shares the pool accepted |
| `board_shares_rejected_total{pool}` | shares the pool rejected |
| `board_stratum_reconnects_total{pool}` | connection attempts after the first |

The first series is the one the scheduler feeds back, so it is the
split the board delivered. The other three are the pool's view of the
same work.

The share each pool gets of the work, as a fraction, over the board's
whole life:

```promql
sum by (pool) (board_pool_work_shares_total)
  / ignoring (pool) group_left
sum (board_pool_work_shares_total)
```

The fee slot's line settles at `permille / 1000`.

The same share over the last three minutes, which shows the scheduler
at work and reacts to a pool going down:

```promql
sum by (pool) (rate(board_pool_work_shares_total[3m]))
  / ignoring (pool) group_left
sum (rate(board_pool_work_shares_total[3m]))
```

The fee slot is the last session slot of the board, and its series
carry that number. Whether the fee pool accepts what it gets, on a
board with four sessions:

```promql
rate(board_shares_rejected_total{pool="3"}[15m])
  / rate(board_shares_accepted_total{pool="3"}[15m])
```

## Logs

The session for the fee slot logs with the tag of its slot, `pool 3`
on a board with four sessions and `pool 2` on one with three. The lines are
the same as for the operator's sessions: connect, new job, accepted,
rejected, and the teardown on a config change.

## Design

- The fee is a session slot, not a weight. The ratio scheduler splits
  work by operator weights, and a fixed small fraction does not
  survive its clamp and its integer division. `FeeScheduler` wraps it
  and keeps the fee in its own counter.
- The switch is a build-time const, not a cargo feature. A feature
  unifies across every crate of one cargo invocation, and a board's
  slot count is one board's fact. The config crate reads `DEVFEE`
  once, and the slot count, the wire list, the fee slot and the
  scheduler all follow that one const.
- The values are one `FeePool` const. The boards of this tree share
  the Braiins pool, and a product with another server states its own
  const; the config crate builds the entry from whichever it is
  handed.
- The worker name is the product's. The pool reads a layout it agreed
  with the product, so the product builds it, and the config crate
  hands it the operator's pools and keeps nothing of the answer.
- The fee slot is a boot argument of the config task, not a static
  another crate fills. The config module owns it, and the handle is
  the one door to its entry, as for every other config fact.
- The operator cannot edit the fee entry because it is not in the
  list a `POST /api/config` replaces: it is a field of its own on
  `GET`, and a `POST` ignores it. The board builds it from the stored
  config on every read, and the stored config does not know it.
- The fee slot never serves as a backup. A board with no working
  operator pool idles, so the fee is a share of the operator's work
  and not a pool of its own.
- The fee session can carry a second protocol. `run_stratum_session`
  takes a `SessionExtension`, and a product hands its fee slot the
  protocol it speaks to its own server, with `NoExtension` on every
  operator slot. The open tree carries the frames and reads none of
  them.
- A fee pool outage is repaid when it returns, up to a cap of 1000
  fee shares. That is the bound the ratio scheduler puts on an
  operator pool's repayment. A longer outage is forgiven past the
  cap. The repayment runs one fee share for every operator share,
  so it is over in twice the shares it owes and never takes the
  whole of the work.
- The fee takes the board's last slot rather than a fixed index, so
  a board that affords three sessions runs the fee at no RAM cost
  and gives up one backup pool instead. A one-slot board runs the
  operator split alone, whatever the switch: the fee is a share of
  the operator's work and needs an operator slot beside it.
- The dashboard shows no card for the fee slot. Its state is in the
  metrics and in `/api/info`, above.
