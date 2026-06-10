//! Live network reconfiguration with confirm-or-revert.
//!
//! Applying a static IP at runtime can strand the device if the address
//! is wrong, so a change is applied to the live stack but held pending
//! until the operator proves reachability by confirming from the new
//! address. Unconfirmed changes revert after [`CONFIRM_WINDOW`]; the
//! pending state lives only in RAM, so a reboot reverts too (boot uses
//! the persisted config, which is only updated on confirm).
//!
//! When a request sets `confirm_after_switch` to `false`, the change is
//! applied live and persisted immediately with no revert window.

use embassy_futures::select::{select, Either};
use embassy_net::{ConfigV4, DhcpConfig, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use portable_atomic::{AtomicBool, AtomicU64, Ordering};
use ubos_config::{Message, NetworkConfig, NetworkMode};
use ubos_logging::{info, warn};

type CSMutex = CriticalSectionRawMutex;

/// How long an unconfirmed live network change stays applied before revert.
const CONFIRM_WINDOW: Duration = Duration::from_secs(120);

/// Commands from the REST handlers to the apply task.
enum Command {
    /// Apply a new network config live. When `confirm_after_switch` is true,
    /// arm the confirm-or-revert window; otherwise persist immediately with
    /// no revert.
    Apply {
        /// The new network configuration to apply.
        network: NetworkConfig,
        /// When `true`, arm the 120 s confirm-or-revert window. When
        /// `false`, apply and persist immediately without a revert window.
        confirm_after_switch: bool,
    },
    /// Confirm the pending change, committing it to flash.
    Confirm,
}

/// Channel from REST handlers to the apply task.
static COMMANDS: Channel<CSMutex, Command, 2> = Channel::new();

/// Uptime-seconds deadline of the pending change, or 0 when none is pending.
static PENDING_DEADLINE_SECS: AtomicU64 = AtomicU64::new(0);

/// Whether the pending preview selects static addressing. Only meaningful
/// while a change is pending; it lets the GET status report the previewed
/// mode instead of the still-persisted one, so mode agrees with the live
/// address during the confirm window.
static PENDING_IS_STATIC: AtomicBool = AtomicBool::new(false);

/// Submit a network change. With `confirm_after_switch`, arm the
/// confirm-or-revert window; otherwise apply and persist immediately.
///
/// Non-blocking; returns `false` when the queue is full so the caller can
/// surface the failure instead of silently dropping the request.
#[must_use]
pub fn apply(network: NetworkConfig, confirm_after_switch: bool) -> bool {
    COMMANDS
        .try_send(Command::Apply {
            network,
            confirm_after_switch,
        })
        .is_ok()
}

/// Confirm the pending change, committing it to flash.
///
/// Non-blocking; returns `false` when the queue is full so the caller can
/// surface the failure instead of silently dropping the request.
#[must_use]
pub fn confirm() -> bool {
    COMMANDS.try_send(Command::Confirm).is_ok()
}

/// Seconds left in the confirm window, or 0 when nothing is pending.
#[must_use]
pub fn pending_seconds_remaining() -> u64 {
    let deadline = PENDING_DEADLINE_SECS.load(Ordering::Relaxed);
    if deadline == 0 {
        return 0;
    }
    deadline.saturating_sub(Instant::now().as_secs())
}

/// Mode of the pending preview, or `None` when nothing is pending.
///
/// `Some(true)` while a static preview awaits confirmation, `Some(false)`
/// for a DHCP preview. Lets the GET status report the previewed mode so it
/// agrees with the live address during the confirm window.
#[must_use]
pub fn pending_mode_is_static() -> Option<bool> {
    if pending_seconds_remaining() == 0 {
        return None;
    }
    Some(PENDING_IS_STATIC.load(Ordering::Relaxed))
}

/// Translate a [`NetworkConfig`] into the stack's [`ConfigV4`].
fn to_config_v4(network: &NetworkConfig) -> ConfigV4 {
    match &network.mode {
        NetworkMode::Static {
            address,
            prefix_len,
            gateway,
            dns,
        } => ubos_net::config::static_ipv4(*address, *prefix_len, *gateway, dns).ipv4,
        NetworkMode::Dhcp => embassy_net::Config::dhcpv4(DhcpConfig::default()).ipv4,
    }
}

/// Pending unconfirmed change.
///
/// Carries the config to persist on confirm, the previous [`ConfigV4`]
/// to restore on revert, and the revert deadline.
struct Pending {
    /// New network config to persist when the operator confirms.
    network: NetworkConfig,
    /// Stack config to restore when the window expires without a confirm.
    fallback: ConfigV4,
    /// Uptime instant after which the change reverts automatically.
    deadline: Instant,
}

/// Drive live network apply with confirm-or-revert.
///
/// Spawn once per board after the stack is created; never returns. The
/// task waits for [`apply`] commands from the REST handler, applies
/// them to the live stack, and either holds a [`Pending`] state until
/// [`confirm`] arrives or the [`CONFIRM_WINDOW`] expires
/// (`confirm_after_switch = true`), or persists immediately with no
/// revert window (`confirm_after_switch = false`).
pub async fn run_network_apply(stack: Stack<'static>) -> ! {
    // The stack was already configured at boot from the persisted config.
    let mut applied = to_config_v4(&ubos_config::handle().current().network);
    let mut pending: Option<Pending> = None;

    loop {
        let command = match &pending {
            None => COMMANDS.receive().await,
            Some(state) => {
                let deadline = state.deadline;
                match select(COMMANDS.receive(), Timer::at(deadline)).await {
                    Either::First(command) => command,
                    Either::Second(()) => {
                        if let Some(expired) = pending.take() {
                            stack.set_config_v4(expired.fallback.clone());
                            applied = expired.fallback;
                            PENDING_DEADLINE_SECS.store(0, Ordering::Relaxed);
                            warn!("network: change not confirmed, reverted");
                        }
                        continue;
                    }
                }
            }
        };

        match command {
            Command::Apply {
                network,
                confirm_after_switch: true,
            } => {
                // Keep the original fallback when superseding a pending change
                // so revert goes all the way back.
                let fallback = pending
                    .take()
                    .map_or_else(|| applied.clone(), |previous| previous.fallback);
                applied = to_config_v4(&network);
                stack.set_config_v4(applied.clone());
                let deadline = Instant::now() + CONFIRM_WINDOW;
                PENDING_IS_STATIC.store(
                    matches!(network.mode, NetworkMode::Static { .. }),
                    Ordering::Relaxed,
                );
                // Round the deadline up so the reported seconds stay
                // positive until the full-precision timer actually fires.
                PENDING_DEADLINE_SECS.store(deadline.as_millis().div_ceil(1000), Ordering::Relaxed);
                info!("network: preview applied, awaiting confirm");
                pending = Some(Pending {
                    network,
                    fallback,
                    deadline,
                });
            }
            Command::Apply {
                network,
                confirm_after_switch: false,
            } => {
                // Apply live and persist immediately; no revert window.
                pending = None;
                applied = to_config_v4(&network);
                stack.set_config_v4(applied.clone());
                PENDING_DEADLINE_SECS.store(0, Ordering::Relaxed);
                PENDING_IS_STATIC.store(false, Ordering::Relaxed);
                ubos_config::handle()
                    .send(Message::SetNetwork(network))
                    .await;
                info!("network: change applied and persisted");
            }
            Command::Confirm => {
                if let Some(confirmed) = pending.take() {
                    ubos_config::handle()
                        .send(Message::SetNetwork(confirmed.network))
                        .await;
                    PENDING_DEADLINE_SECS.store(0, Ordering::Relaxed);
                    PENDING_IS_STATIC.store(false, Ordering::Relaxed);
                    info!("network: change confirmed");
                } else {
                    info!("network: confirm with nothing pending");
                }
            }
        }
    }
}
