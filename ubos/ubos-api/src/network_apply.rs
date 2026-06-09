//! Live network reconfiguration with confirm-or-revert.
//!
//! Applying a static IP at runtime can strand the device if the address
//! is wrong, so a change is applied to the live stack but held pending
//! until the operator proves reachability by confirming from the new
//! address. Unconfirmed changes revert after [`CONFIRM_WINDOW`]; the
//! pending state lives only in RAM, so a reboot reverts too (boot uses
//! the persisted config, which is only updated on confirm).

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
    /// Apply a new network config live and start the confirm window.
    Preview(NetworkConfig),
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

/// Submit a network change to apply live and hold pending.
///
/// Non-blocking; drops the request when the queue is full. The operator
/// can retry if the drop happens.
pub fn preview(network: NetworkConfig) {
    let _ = COMMANDS.try_send(Command::Preview(network));
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
/// task waits for [`preview`] commands from the REST handler, applies
/// them to the live stack, and holds a [`Pending`] state until either
/// [`confirm`] arrives or the [`CONFIRM_WINDOW`] expires.
pub async fn run_network_apply(stack: Stack<'static>) -> ! {
    // The stack was already configured at boot from the persisted config.
    let mut applied = to_config_v4(&ubos_config::handle().current().network);
    let mut pending: Option<Pending> = None;

    loop {
        match &pending {
            None => match COMMANDS.receive().await {
                Command::Preview(network) => {
                    let fallback = applied.clone();
                    applied = to_config_v4(&network);
                    stack.set_config_v4(applied.clone());
                    let deadline = Instant::now() + CONFIRM_WINDOW;
                    PENDING_IS_STATIC.store(
                        matches!(network.mode, NetworkMode::Static { .. }),
                        Ordering::Relaxed,
                    );
                    PENDING_DEADLINE_SECS.store(deadline.as_secs(), Ordering::Relaxed);
                    info!("network: preview applied, awaiting confirm");
                    pending = Some(Pending {
                        network,
                        fallback,
                        deadline,
                    });
                }
                Command::Confirm => {
                    info!("network: confirm with nothing pending");
                }
            },
            Some(state) => {
                match select(COMMANDS.receive(), Timer::at(state.deadline)).await {
                    Either::First(Command::Confirm) => {
                        if let Some(confirmed) = pending.take() {
                            ubos_config::handle()
                                .send(Message::SetNetwork(confirmed.network))
                                .await;
                            PENDING_DEADLINE_SECS.store(0, Ordering::Relaxed);
                            info!("network: change confirmed");
                        }
                    }
                    Either::First(Command::Preview(network)) => {
                        // A newer preview supersedes the current one; keep
                        // the original fallback so revert goes all the way back.
                        if let Some(previous) = pending.take() {
                            applied = to_config_v4(&network);
                            stack.set_config_v4(applied.clone());
                            let deadline = Instant::now() + CONFIRM_WINDOW;
                            PENDING_IS_STATIC.store(
                                matches!(network.mode, NetworkMode::Static { .. }),
                                Ordering::Relaxed,
                            );
                            PENDING_DEADLINE_SECS.store(deadline.as_secs(), Ordering::Relaxed);
                            pending = Some(Pending {
                                network,
                                fallback: previous.fallback,
                                deadline,
                            });
                        }
                    }
                    Either::Second(()) => {
                        if let Some(expired) = pending.take() {
                            stack.set_config_v4(expired.fallback.clone());
                            applied = expired.fallback;
                            PENDING_DEADLINE_SECS.store(0, Ordering::Relaxed);
                            warn!("network: change not confirmed, reverted");
                        }
                    }
                }
            }
        }
    }
}
