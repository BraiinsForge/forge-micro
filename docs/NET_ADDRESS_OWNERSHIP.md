# Network address ownership

Status: implemented — the code wins. When this file and the code
disagree, the code is right and this file needs a patch.

Which crate owns the network address, the confirm-or-revert window, and
the live stack reconfiguration. The split exists so that `ubos-api` stays
a thin public interface rather than the owner of a live state machine,
and so the assigned IPv4 address is observed in exactly one place.

## Ownership split

- **`ubos-config`** owns intent and the confirm-or-revert window. It runs
  the timer, exposes the pending status, and fires the
  `network_applied` / `network_reverted` events.
- **`ubos-net`** is the single owner of IP assignment. It is the only
  crate that calls `set_config_v4` and the only crate that observes the
  assigned address. It reacts to config; it decides no policy.
- **`ubos-api`** is a thin forwarder. It translates HTTP requests into
  `ubos-config` commands and reports status read back from config. It
  holds no network state machine.

## `ubos-config`: intent and the revert target

Config keeps the committed config and an unconfirmed preview apart with
two watches, not a literal `actual`/`backup` pair:

- `STATE_WATCH` / `current()` always carry the **committed** config.
  `committed.network` is the revert target, untouched while a preview is
  armed, so an `/api/config` update built from `current()` never
  persists a preview.
- `NETWORK_WATCH<NetworkConfig>` carries the **effective** network: the
  preview while a confirm window is armed, otherwise the committed
  network. `ubos-net` subscribes to this watch.

Persistence rule: **flash is written only on confirm.** The preview lives
only on `NETWORK_WATCH`; flash holds the last confirmed config; boot
loads flash into the committed config. A reboot during the confirm
window therefore comes up on the last confirmed config — the
reboot-reverts safety holds for free, with no flash write on an
unconfirmed preview.

`ubos-config` publishes a newly-applied preview onto `NETWORK_WATCH`
immediately; it never blocks on a timer. The hold-off that lets the HTTP
ack flush before the stack switches lives in `ubos-net` (see below).

## Confirm-or-revert, by mode

Config runs the timer and is agnostic about who confirms. On an apply
that needs confirmation it arms a window and waits for a
`Message::ConfirmNetwork` on the existing command channel:

- arrives in time → promote the preview (write flash).
- times out → republish `committed.network` on `NETWORK_WATCH`;
  `ubos-net` sees the change and switches the stack back.

The confirmation source is forced by the change mode:

- **Static target → operator confirms via `ubos-api`.** A wrong static IP
  strands the box. `ubos-net` setting the address proves nothing — the
  stack "has" the address immediately regardless of reachability. The
  only valid proof is a request that had to traverse the new config to
  arrive: the operator's browser confirming from the new address.
- **DHCP target → `ubos-net` confirms on lease.** A lease is genuine
  evidence the DHCP path works, so `run_network_control` sends
  `ConfirmNetwork` once a lease lands; no operator action is needed, and
  config reverts on its own if no lease arrives.

Same command, same channel, different sender.

## `ubos-net`: the stateless mechanic

`run_network_control` is one long-lived task, spawned on every board
beside the existing net tasks, taking `Stack<'static>`:

1. Subscribe to `NETWORK_WATCH`. On a genuine change, wait
   `REPLY_FLUSH_DELAY` (~1 s) so the API handler's HTTP ack flushes
   before `set_config_v4` tears down the connection the request arrived
   on, then switch the stack and call `ubos_auth::clear_all_sessions`. A
   deliberate reconfiguration invalidates each session's CSRF token and
   lands the SPA on `/login` — the unambiguous "it took effect" signal.
   No fallback state is held in net; the revert target lives in config.
2. Poll `stack.config_v4()` and publish the assigned address via
   `Handle::set_ipv4`, which records `ip_assigned` once per genuine
   change.
3. For a DHCP-mode change, once a lease is observed, send
   `ConfirmNetwork` to config. Config no-ops a confirm with nothing
   pending, so a boot lease is harmless.

`wait_for_config` stays as a boot barrier — the firmware logs the first
address and `run_ntp_sync` awaits a first lease. It still calls
`set_ipv4` on its poll: the call is idempotent (`ip_assigned` fires only
on a genuine address change), so the boot barrier and the observer task
publishing the same address is safe.

## `ubos-api`: the forwarder

- `apply` / `confirm` / `revert` are `config.try_send(Message::...)`.
- `pending_seconds_remaining` and `pending_mode_is_static` live in
  config, which owns the timer; the REST status handlers read them back.
- `network_apply.rs`, `run_network_apply`, and `reconcile_local_ipv4`
  are gone, and firmware no longer spawns `run_network_apply`.

## Events

`network_applied` and `network_reverted` belong to `ubos-config`, beside
the state machine that decides them. `ip_assigned` belongs to `ubos-net`,
fired solely from the observer task. The record format and the design
rationale are in `ubos/ubos-events/src/lib.rs`. The served catalog is
`GET /api/system/events/catalog`, described in `docs/REST_API.md`.

## Dependencies

`ubos-net` depends on `ubos-config` (to subscribe to `NETWORK_WATCH`) and
on `ubos-auth` (to clear sessions on a switch). Both arrows are acyclic:
`ubos-config` depends only on `ubos-core`, `ubos-events`, and
`ubos-logging`, and never reaches net. This holds as long as
`ubos-config` stays the bottom store.
