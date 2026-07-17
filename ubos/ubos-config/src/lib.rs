//! Mining firmware configuration schema and distribution handle.
//!
//! Defines the [`Config`] type that carries all runtime-tunable
//! parameters and the [`Handle`] consumers use to read the current
//! config and wait for changes.
//!
//! Firmware spawns [`run`], which publishes [`Config::default`] once
//! at boot and then drains [`COMMAND_CHANNEL`] forever. On each
//! [`Message::Update`] it persists and publishes the new value on
//! [`STATE_WATCH`].

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

mod message;

#[cfg(test)]
#[path = "tests/storage.rs"]
mod tests_storage;

#[cfg(test)]
#[path = "tests/validation.rs"]
mod tests_validation;

#[cfg(all(test, feature = "std"))]
#[path = "tests/factory_reset.rs"]
mod tests_factory_reset;

pub mod api;
pub mod cooling;
pub mod miner;
pub mod mining;
pub mod network;
pub mod pool;
pub mod thermal;

use ubos_asic::HashboardDescriptor;

pub use api::ApiConfig;
pub use cooling::{
    BOUNDS as COOLING_BOUNDS, CoolingBounds, CoolingConfig, CoolingInfo, INFO as COOLING_INFO,
    PidConfig,
};
pub use miner::{MinerConfig, TuningAlgorithm};
pub use mining::{
    BOUNDS as MINING_BOUNDS, MiningBounds, MiningInfo, TuningConfig, bounds, mining_info,
};
pub use network::{NetworkConfig, NetworkMode};
pub use pool::{
    DEFAULT_POOL_HOST, DEFAULT_POOL_PORT, DEFAULT_POOL_SV2_PUBKEY, DEFAULT_POOL_USER,
    MAX_POOL_WEIGHT, PoolEntry, ProtocolKind,
};
pub use thermal::{
    BOUNDS as THERMAL_BOUNDS, INFO as THERMAL_INFO, ThermalBounds, ThermalConfig, ThermalInfo,
};

pub mod storage;
pub use storage::{CONFIG_KEY, LoadError, Storage, decode_blob, encode_blob};

pub mod flash_storage;
pub use flash_storage::FlashStorage;

pub mod sync_flash;
pub use sync_flash::SyncFlash;

#[cfg(feature = "std")]
pub mod file_storage;
#[cfg(feature = "std")]
pub use file_storage::FileStorage;

pub use message::Message;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Timer};
use portable_atomic::{AtomicBool, AtomicU64, Ordering};
use ubos_logging::{info, warn};

ubos_events::events! {
    config_changed(freq_mhz: u32, voltage_mv: u32)  info = "Configuration changed: {} MHz, {} mV",
    factory_reset()                                  warn = "Factory reset to defaults",
    network_applied()                                info = "Network config applied",
    network_reverted()                               warn = "Network config reverted",
}

/// Subscriber capacity for the config watch.
///
/// Covers thermal, cooling, mining/tuner, manager, API, and stratum
/// client, with two slots spare for future consumers.
const WATCH_CAPACITY: usize = 8;

/// Depth of the config command channel.
const COMMAND_CHANNEL_DEPTH: usize = 4;

/// Stack buffer size for encode and decode of stored config blobs.
///
/// Holds the 8-byte schema-hash prefix plus the postcard-encoded
/// [`Config`]. The factory-default blob is ~111 bytes (see the golden
/// snapshot in `src/tests/storage.rs`); 512 leaves room for the pool list
/// and network section to grow without re-tuning.
/// [`flash_storage::SCRATCH_SIZE`] is a const alias for this value, so
/// raising it here raises the flash scratch budget in lockstep.
pub(crate) const LOAD_BUFFER_SIZE: usize = 512;

type CSMutex = CriticalSectionRawMutex;

/// Module-internal state watch carrying the committed [`Config`] —
/// the persisted truth, never the unconfirmed network preview. Single
/// writer is [`run`]. `current()` reads this, so an `/api/config`
/// update built from `current()` never carries a pending preview into
/// flash.
pub(crate) static STATE_WATCH: Watch<CSMutex, Config, WATCH_CAPACITY> = Watch::new();

/// Subscriber capacity for the effective-network watch. `ubos-net` is
/// the sole subscriber; one spare slot covers a future consumer.
const NETWORK_WATCH_CAPACITY: usize = 2;

/// Module-internal watch carrying the effective network configuration —
/// the value the live stack should hold, which is the unconfirmed
/// preview while a confirm window is armed and the committed network
/// otherwise. `ubos-net` subscribes and applies each change to the
/// stack. Distinct from [`STATE_WATCH`] precisely so the preview reaches
/// the stack without poisoning the persisted config. Published only on a
/// network transition, never at boot — the boot stack config comes from
/// firmware reading [`Config`] before the stack exists.
pub(crate) static NETWORK_WATCH: Watch<CSMutex, NetworkConfig, NETWORK_WATCH_CAPACITY> =
    Watch::new();

/// Module-internal command channel. Single consumer is [`run`];
/// producers send via [`Handle::send`].
pub(crate) static COMMAND_CHANNEL: Channel<CSMutex, Message, COMMAND_CHANNEL_DEPTH> =
    Channel::new();

/// Receiver capacity for the update-generation watch: one per pool
/// session task ([`pool::MAX_POOLS`]), one for the pool-weights
/// watcher, plus one spare.
const GENERATION_WATCH_CAPACITY: usize = pool::MAX_POOLS + 2;

/// Update-generation watch. The counter is bumped and published once
/// each time a successful [`Message::Update`] has been persisted and
/// published. Every stratum session task awaits it in its reconnect
/// loop so a pool change triggers an immediate teardown instead of
/// waiting for the next session error. A `Watch` rather than a
/// `Signal` because a `Signal` registers a single waker: with one
/// session task per pool slot, all of them must observe every update.
pub(crate) static GENERATION_WATCH: Watch<CSMutex, u32, GENERATION_WATCH_CAPACITY> = Watch::new();

/// How long an unconfirmed network preview stays applied before it
/// reverts to the committed config. A wrong static address strands the
/// board, so the change holds only until the operator proves
/// reachability by confirming from the new address.
const CONFIRM_WINDOW: Duration = Duration::from_secs(120);

/// Uptime-seconds deadline of the pending network preview, or 0 when
/// none is pending. Written by [`run`], read synchronously by the API
/// HTTP handlers, which cannot await the config task.
static PENDING_DEADLINE_SECS: AtomicU64 = AtomicU64::new(0);

/// Whether the pending preview selects static addressing. Only
/// meaningful while a preview is armed; lets the network status report
/// the previewed mode so it agrees with the live address during the
/// window.
static PENDING_IS_STATIC: AtomicBool = AtomicBool::new(false);

/// Seconds left in the network confirm window, or 0 when nothing is
/// pending.
#[must_use]
pub fn pending_seconds_remaining() -> u64 {
    let deadline = PENDING_DEADLINE_SECS.load(Ordering::Relaxed);
    if deadline == 0 {
        return 0;
    }
    deadline.saturating_sub(Instant::now().as_secs())
}

/// Mode of the pending network preview, or `None` when nothing is
/// pending. `Some(true)` while a static preview awaits confirmation,
/// `Some(false)` for a DHCP preview.
#[must_use]
pub fn pending_mode_is_static() -> Option<bool> {
    if pending_seconds_remaining() == 0 {
        return None;
    }
    Some(PENDING_IS_STATIC.load(Ordering::Relaxed))
}

/// Per-task subscription returned by [`Handle::subscribe`].
pub type ConfigSubscription = Receiver<'static, CSMutex, Config, WATCH_CAPACITY>;

/// Per-task subscription to the effective-network watch, returned by
/// [`Handle::subscribe_network`].
pub type NetworkSubscription = Receiver<'static, CSMutex, NetworkConfig, NETWORK_WATCH_CAPACITY>;

/// Per-task subscription to the update-generation watch, returned by
/// [`Handle::subscribe_generation`].
pub type GenerationSubscription = Receiver<'static, CSMutex, u32, GENERATION_WATCH_CAPACITY>;

/// Obtain a handle to the config module. `Copy`; callers freely pass
/// it to multiple tasks.
#[must_use]
pub fn handle() -> Handle {
    Handle {
        state: &STATE_WATCH,
    }
}

/// Consumer-side handle for the config subsystem.
///
/// `Copy` because it holds only a `'static` reference. Every consumer
/// task takes its own value.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static Watch<CSMutex, Config, WATCH_CAPACITY>,
}

impl Handle {
    /// Returns the most recently published config, falling back to
    /// [`Config::default`] if nothing has been published yet.
    #[must_use]
    pub fn current(&self) -> Config {
        // Fall back to Default before the first publish; the window is
        // tiny but must not return garbage.
        self.state.try_get().unwrap_or_default()
    }

    /// Per-task subscription. Call once per consumer task before
    /// entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe(&self) -> ConfigSubscription {
        self.state
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    /// Waits until the config changes and returns the new value.
    pub async fn changed(&self) -> Config {
        let mut subscription = self.subscribe();
        subscription.changed().await
    }

    /// Subscription to the effective-network watch. `ubos-net` calls this
    /// once and applies each published [`NetworkConfig`] to the live
    /// stack. The watch publishes only on a network transition, so the
    /// first `changed()` blocks until the first apply/confirm/revert.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe_network(&self) -> NetworkSubscription {
        NETWORK_WATCH
            .receiver()
            .expect("BUG: NETWORK_WATCH receiver slots exhausted; raise NETWORK_WATCH_CAPACITY")
    }

    /// Submit a config update. The task running in [`run`] persists the
    /// new value then publishes it on `STATE_WATCH`.
    pub async fn send(&self, message: Message) {
        COMMAND_CHANNEL.send(message).await;
    }

    /// Submit a command without awaiting queue space. Returns `false`
    /// when the channel is full so a non-async caller, such as an HTTP
    /// handler, can surface a busy response instead of blocking.
    #[must_use]
    pub fn try_send(&self, message: Message) -> bool {
        COMMAND_CHANNEL.try_send(message).is_ok()
    }

    /// Per-task subscription to the update-generation watch. The
    /// generation is bumped after each successful config update has
    /// been persisted and published. Consumers that need to react
    /// before the next [`Handle::changed`] tick (a stratum session
    /// tearing down, for example) await `changed()` on this receiver.
    /// Call once per consumer task before entering its loop.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `Watch` has no free receiver slots.
    #[must_use]
    pub fn subscribe_generation(&self) -> GenerationSubscription {
        GENERATION_WATCH.receiver().expect(
            "BUG: GENERATION_WATCH receiver slots exhausted; raise GENERATION_WATCH_CAPACITY",
        )
    }
}

/// Partial-update cooling section of [`ConfigUpdate`].
///
/// Only the two operator-tunable cooling fields are exposed; the rest of
/// [`CoolingConfig::pid`] (the PID gains) is factory-set and never patched
/// from the UI. Each field is `Option` so one can be set without the other.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CoolingUpdate {
    /// Replacement for [`CoolingConfig::target_temp_c`]; `None` leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_temp_c: Option<f32>,
    /// Replacement for [`PidConfig::min_fan_pwm`] (the fan floor, 0-100);
    /// `None` leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_fan_pwm: Option<u8>,
}

/// Partial-update wire shape for `GET` and `POST /api/config`.
///
/// All fields are `Option` so callers can patch one section at a time
/// without round-tripping unrelated state. A missing JSON key deserializes
/// as `None`; the POST handler merges only the fields that are `Some` into
/// the running [`Config`]. On `GET`, the response includes all fields with
/// `api.password` blanked to avoid leaking the stored secret.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ConfigUpdate {
    /// Replacement value for [`Config::pool`]; `None` means leave
    /// unchanged. `Some(empty Vec)` is the wire shape for "no pools
    /// configured" and clears all three persisted slots — the webui
    /// produces this when every URL input is blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<heapless::Vec<PoolEntry, { pool::MAX_POOLS }>>,
    /// Replacement value for [`Config::thermal`]; `None` means leave unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal: Option<ThermalConfig>,
    /// Partial cooling update (setpoint and fan floor); `None` means leave
    /// unchanged. The full [`CoolingConfig`] is not exposed because the PID
    /// gains are factory-tuned and must not be overwritten from the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooling: Option<CoolingUpdate>,
    /// Replacement value for [`Config::miner`]; `None` means leave unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miner: Option<MinerConfig>,
    /// Replacement value for [`Config::tuning`]; `None` means leave unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuning: Option<TuningConfig>,
    /// Replacement value for [`Config::api`]; `None` means leave unchanged.
    ///
    /// On `GET`, `password` is set to an empty string so the stored secret
    /// never leaves the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiConfig>,
    /// Replacement value for [`Config::network`]; `None` means leave unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkConfig>,
}

impl ConfigUpdate {
    /// Merge the update into `target`, overwriting only the fields that are
    /// `Some`.
    ///
    /// Consumes `self`; each `Some` field is moved into `target` rather than
    /// cloned, so callers cannot reuse the update afterwards.
    pub fn apply(self, target: &mut Config) {
        if let Some(pool) = self.pool {
            target.pool = pool;
        }
        if let Some(thermal) = self.thermal {
            target.thermal = thermal;
        }
        if let Some(cooling) = self.cooling {
            if let Some(temp) = cooling.target_temp_c {
                target.cooling.target_temp_c = temp;
            }
            if let Some(pwm) = cooling.min_fan_pwm {
                target.cooling.pid.min_fan_pwm = pwm;
            }
        }
        if let Some(miner) = self.miner {
            target.miner = miner;
        }
        if let Some(tuning) = self.tuning {
            target.tuning = tuning;
        }
        if let Some(api) = self.api {
            target.api = api;
        }
        if let Some(network) = self.network {
            target.network = network;
        }
    }

    /// Produce a fully-populated `ConfigUpdate` from a live [`Config`].
    ///
    /// Used by `GET /api/config`. The `api.password` field is set to an empty
    /// string so the stored credential is never transmitted to the client.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let mut api = config.api.clone();
        api.password = heapless::String::new();
        Self {
            pool: Some(config.pool.clone()),
            thermal: Some(config.thermal.clone()),
            cooling: Some(CoolingUpdate {
                target_temp_c: Some(config.cooling.target_temp_c),
                min_fan_pwm: Some(config.cooling.pid.min_fan_pwm),
            }),
            miner: Some(config.miner.clone()),
            tuning: Some(config.tuning.clone()),
            api: Some(api),
            network: Some(config.network.clone()),
        }
    }

    /// Validate all present fields against the bounds and against `current`.
    ///
    /// `current` is the live [`Config`] snapshot the update will be merged
    /// into; the caller is responsible for reading it once and passing it
    /// in so the function is pure and the read order is explicit.
    ///
    /// For `cooling.target_temp_c`, the dangerous-temp limit is resolved
    /// from the incoming `thermal` field when present, otherwise from
    /// `current.thermal.dangerous_temp_c`.
    ///
    /// The cross-field rule covers both directions:
    /// - When `cooling.target_temp_c` is in the update, the incoming target
    ///   must be strictly less than the incoming (or current) `dangerous_temp_c`.
    /// - When only `thermal` is in the update, the current `cooling.target_temp_c`
    ///   must still be strictly less than the incoming `dangerous_temp_c`.
    ///
    /// Returns `Ok(())` when every present field is within its allowed range.
    /// Returns `Err(msg)` with a static, operator-readable message on the
    /// first violation found.
    ///
    /// The `cooling.pid` gains are deliberately not part of [`ConfigUpdate`]
    /// and therefore never flow through this validator; see the parallel
    /// note on [`Config::validate`].
    ///
    /// # Errors
    ///
    /// Returns `Err` on the first field that fails its validation rule.
    pub fn validate(&self, current: &Config, bounds: &MiningBounds) -> Result<(), &'static str> {
        if let Some(pool) = &self.pool {
            for (slot, entry) in pool.iter().enumerate() {
                entry.validate(slot)?;
            }
        }
        if let Some(thermal) = &self.thermal {
            thermal.validate()?;
        }
        if let Some(miner) = &self.miner {
            miner.validate()?;
        }
        if let Some(tuning) = &self.tuning {
            tuning.validate(bounds)?;
        }
        if let Some(api) = &self.api {
            api.validate()?;
        }
        if let Some(network) = &self.network {
            network.validate()?;
        }
        let cooling_target = self.cooling.as_ref().and_then(|c| c.target_temp_c);
        if let Some(pwm) = self.cooling.as_ref().and_then(|c| c.min_fan_pwm) {
            // PID output clamps to [min_fan_pwm, 100]; a floor above 100 would
            // invert the clamp range (see ubos-cooling::pid::set_limits).
            if pwm > 100 {
                return Err("cooling.min_fan_pwm must be between 0 and 100");
            }
        }

        if self.thermal.is_some() || cooling_target.is_some() {
            let dangerous = self
                .thermal
                .as_ref()
                .map_or(current.thermal.dangerous_temp_c, |thermal| {
                    thermal.dangerous_temp_c
                });

            if let Some(target) = cooling_target {
                CoolingConfig::validate_target_temp(target, dangerous)?;
            } else if current.cooling.target_temp_c >= dangerous {
                return Err(
                    "current cooling.target_temp_c must remain less than thermal.dangerous_temp_c",
                );
            }
        }

        Ok(())
    }
}

/// All runtime-tunable firmware parameters.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Config {
    /// Ordered list of pool endpoints; the client tries them in order.
    pub pool: heapless::Vec<PoolEntry, { pool::MAX_POOLS }>,
    /// Thermal monitoring parameters.
    pub thermal: ThermalConfig,
    /// Cooling loop parameters.
    pub cooling: CoolingConfig,
    /// Miner orchestration (tuner algorithm selection).
    pub miner: MinerConfig,
    /// ASIC tuning parameters.
    pub tuning: TuningConfig,
    /// HTTP API credentials.
    pub api: ApiConfig,
    /// Network interface configuration (IP assignment mode and hostname).
    pub network: NetworkConfig,
}

impl Default for Config {
    /// Factory defaults for the BMM101 board.
    ///
    /// Provided so the test suite (and any caller without a specific
    /// board) has a stable baseline; the firmware uses
    /// [`Config::for_board`] with its own descriptor instead.
    fn default() -> Self {
        Self::for_board(&ubos_asic::BMM101_HASHBOARD)
    }
}

impl Config {
    /// Factory defaults for `board`. The tuning operating point falls back
    /// to the board's boot frequency/voltage when the `MINING_*` build env
    /// is unset, so a board comes up at its correct rail/clock.
    ///
    /// Not a `const fn` because [`ApiConfig`]'s default cannot be `const`
    /// (heapless 0.9 `String::push_str` is not `const fn`). The value is
    /// computed once at boot; it is cheap.
    #[must_use]
    pub fn for_board(board: &'static HashboardDescriptor) -> Self {
        let mut host = heapless::String::new();
        host.push_str(option_env!("POOL_HOST").unwrap_or(DEFAULT_POOL_HOST))
            .expect("BUG: POOL_HOST exceeds PoolEntry::host capacity");
        let mut user = heapless::String::new();
        user.push_str(option_env!("POOL_USER").unwrap_or(DEFAULT_POOL_USER))
            .expect("BUG: POOL_USER exceeds PoolEntry::user capacity");
        let port = option_env!("POOL_PORT")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(DEFAULT_POOL_PORT);
        let entry = PoolEntry {
            host,
            port,
            user,
            protocol: ProtocolKind::Sv2 {
                pool_pubkey: Some(DEFAULT_POOL_SV2_PUBKEY),
            },
            weight: 0,
        };
        let mut pool = heapless::Vec::new();
        pool.push(entry)
            .expect("BUG: single default pool entry exceeds Config::pool capacity");
        Self {
            pool,
            thermal: ThermalConfig::default(),
            cooling: CoolingConfig {
                // The board's cooling profile lives on its descriptor: the
                // hashnode runs die-temp blind (synthetic pcb+offset), so it
                // carries a lower setpoint and a higher fan floor than the
                // default board. Runtime-editable via the UI afterwards.
                target_temp_c: board.cooling.target_temp_c,
                pid: PidConfig {
                    min_fan_pwm: board.cooling.min_fan_pwm,
                    ..PidConfig::default()
                },
            },
            miner: MinerConfig {
                // Fixed is the only available algorithm.
                algorithm: TuningAlgorithm::Fixed,
            },
            tuning: TuningConfig {
                // The board's boot operating point comes from its descriptor,
                // so a board comes up at its correct rail/clock with no env.
                frequency_mhz: board.default_frequency.mhz(),
                voltage_mv: board.default_voltage.mv(),
            },
            api: ApiConfig::default(),
            network: NetworkConfig::default(),
        }
    }
}

impl Config {
    /// Compile-time schema fingerprint for the [`Config`] type.
    ///
    /// Computed via FNV1a-64 over the postcard schema and the type path
    /// string. Any field addition, removal, rename, reorder, or type
    /// change shifts this hash. [`crate::storage::decode_blob`] compares
    /// the 8-byte prefix of stored blobs against this constant and
    /// returns [`crate::storage::LoadError::SchemaMismatch`] on mismatch,
    /// allowing the firmware to fall back to defaults safely.
    pub const SCHEMA_HASH: [u8; 8] =
        postcard_schema::key::hash::fnv1a64::hash_ty_path::<Config>("ubos_config::Config");

    /// Validate operator-tunable fields against their per-domain bounds.
    ///
    /// Runs the pool, thermal, miner, tuning, and api sub-validators and
    /// enforces the cross-field rule
    /// `cooling.target_temp_c < thermal.dangerous_temp_c`. Stored blobs
    /// pass through serde and can carry out-of-range or non-finite floats
    /// in those fields; [`load_initial`] calls this after [`decode_blob`]
    /// so a corrupted blob falls back to defaults instead of feeding
    /// consumers values that bypass per-update validation.
    ///
    /// The `cooling.pid` gains are not user-tunable through [`ConfigUpdate`]
    /// and therefore only enter flash via [`Config::default`]; they are not
    /// re-validated here.
    ///
    /// # Errors
    ///
    /// Returns the first failing field's static error string.
    pub fn validate(&self, bounds: &MiningBounds) -> Result<(), &'static str> {
        for (slot, entry) in self.pool.iter().enumerate() {
            entry.validate(slot)?;
        }
        self.thermal.validate()?;
        self.miner.validate()?;
        self.tuning.validate(bounds)?;
        self.api.validate()?;
        self.network.validate()?;
        CoolingConfig::validate_target_temp(
            self.cooling.target_temp_c,
            self.thermal.dangerous_temp_c,
        )?;
        Ok(())
    }
}

/// Reconfigure task for the config subsystem.
///
/// On boot, loads the stored [`Config`] via `storage`. On
/// [`LoadError::SchemaMismatch`] (firmware upgraded across an
/// incompatible schema), [`LoadError::Corrupt`] (partial write before
/// power loss), or an I/O error, falls back to [`Config::default`] and
/// logs the reason. `Ok(None)` means the key was never written and is
/// treated as a clean first-boot — defaults, no warning.
///
/// Each [`Message::Update`] persists the new value before publishing
/// it and bumps [`GENERATION_WATCH`] so every stratum session tears
/// down and reconnects.
///
/// Network changes flow through [`Message::ApplyNetwork`]. With
/// `confirm`, the new network is published on [`NETWORK_WATCH`] as a
/// preview and a [`CONFIRM_WINDOW`] timer is armed, but nothing is
/// persisted: `ubos-net` switches the live stack, and the change holds
/// only until [`Message::ConfirmNetwork`] arrives or the window expires
/// and the committed network is republished. Because the preview never
/// touches flash, a reboot mid-window comes up on the committed config.
/// `committed.network` is the revert target and is left untouched while
/// a preview is pending, so a superseding apply still reverts all the
/// way back.
pub async fn run<S: Storage>(mut storage: S, board: &'static HashboardDescriptor) -> ! {
    let mut committed = load_initial(&mut storage, board).await;
    STATE_WATCH.sender().send(committed.clone());
    let mut preview: Option<(NetworkConfig, Instant)> = None;
    info!("config: started");

    loop {
        // While a preview is armed, race the next command against the
        // revert deadline; otherwise just await the next command.
        let message = if let Some(&(_, deadline)) = preview.as_ref() {
            match select(COMMAND_CHANNEL.receive(), Timer::at(deadline)).await {
                Either::First(message) => message,
                Either::Second(()) => {
                    preview = None;
                    clear_pending_status();
                    NETWORK_WATCH.sender().send(committed.network.clone());
                    warn!("config: network change not confirmed, reverted");
                    crate::events::network_reverted();
                    continue;
                }
            }
        } else {
            COMMAND_CHANNEL.receive().await
        };

        match message {
            Message::Update(new) => {
                // `new` was built from `current()`, which is the committed
                // config, so it never carries the pending preview into flash.
                apply_update(&new, &mut storage).await;
                committed = new;
            }
            Message::ApplyNetwork { network, confirm } => {
                if confirm {
                    let deadline = Instant::now() + CONFIRM_WINDOW;
                    arm_pending_status(&network, deadline);
                    NETWORK_WATCH.sender().send(network.clone());
                    preview = Some((network, deadline));
                    info!("config: network preview applied, awaiting confirm");
                } else {
                    preview = None;
                    clear_pending_status();
                    committed.network = network;
                    set_network(&committed, &mut storage).await;
                    NETWORK_WATCH.sender().send(committed.network.clone());
                    crate::events::network_applied();
                }
            }
            Message::ConfirmNetwork => {
                if let Some((network, _)) = preview.take() {
                    clear_pending_status();
                    committed.network = network;
                    // The stack already holds the preview; net dedups, so no
                    // NETWORK_WATCH publish is needed, only the persisted record.
                    set_network(&committed, &mut storage).await;
                    info!("config: network change confirmed");
                    crate::events::network_applied();
                } else {
                    info!("config: confirm network with nothing pending");
                }
            }
            Message::RevertNetwork => {
                if preview.take().is_some() {
                    clear_pending_status();
                    NETWORK_WATCH.sender().send(committed.network.clone());
                    info!("config: network change reverted by operator");
                    crate::events::network_reverted();
                } else {
                    info!("config: revert network with nothing pending");
                }
            }
            Message::FactoryReset => {
                preview = None;
                clear_pending_status();
                committed = factory_reset(&mut storage, board).await;
                NETWORK_WATCH.sender().send(committed.network.clone());
                ubos_core::reboot::request_reboot();
            }
        }
    }
}

/// Arm the pending-window status counters read by the API handlers.
fn arm_pending_status(network: &NetworkConfig, deadline: Instant) {
    // Publish the mode before the deadline. A reader that observes a nonzero
    // deadline then always sees the matching mode, never the prior preview's.
    PENDING_IS_STATIC.store(
        matches!(network.mode, NetworkMode::Static { .. }),
        Ordering::Relaxed,
    );
    // Round the deadline up so the reported seconds stay positive until
    // the full-precision timer actually fires.
    PENDING_DEADLINE_SECS.store(deadline.as_millis().div_ceil(1000), Ordering::Relaxed);
}

/// Clear the pending-window status counters once no preview is armed.
fn clear_pending_status() {
    PENDING_DEADLINE_SECS.store(0, Ordering::Relaxed);
    PENDING_IS_STATIC.store(false, Ordering::Relaxed);
}

/// Load the persisted [`Config`] or fall back to [`Config::default`].
///
/// The buffer is sized to match [`encode_blob`]'s budget so any blob
/// that could have been stored fits without reallocation. All failure
/// paths converge on defaults so the firmware always boots with a
/// valid config.
async fn load_initial<S: Storage>(storage: &mut S, board: &'static HashboardDescriptor) -> Config {
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let blob = match storage.load(CONFIG_KEY, &mut buffer).await {
        Ok(Some(blob)) => blob,
        Ok(None) => {
            info!("config: no stored config, using defaults");
            return Config::for_board(board);
        }
        Err(_) => {
            warn!("config: load failed, using defaults");
            return Config::for_board(board);
        }
    };
    let config = match decode_blob::<S::Error>(blob) {
        Ok(config) => config,
        Err(error) => {
            log_decode_error(&error);
            return Config::for_board(board);
        }
    };
    if config.validate(&mining::bounds(board)).is_err() {
        warn!("config: stored config failed validation, using defaults");
        return Config::for_board(board);
    }
    info!("config: loaded from storage");
    config
}

/// Log the variant-specific reason a stored blob could not be decoded.
///
/// Each arm produces a different diagnostic so operators can tell a
/// firmware-upgrade schema bump apart from a flash corruption event.
#[allow(clippy::match_same_arms)]
fn log_decode_error<E>(error: &LoadError<E>) {
    match error {
        LoadError::SchemaMismatch { .. } => warn!("config: schema mismatch, using defaults"),
        LoadError::Corrupt => warn!("config: stored blob corrupt, using defaults"),
        LoadError::Io(_) => warn!("config: storage error decoding blob, using defaults"),
    }
}

/// Apply a config update with persistence and stratum restart.
///
/// Persists the new value, publishes it on [`STATE_WATCH`], and bumps
/// the update generation so stratum sessions reconnect. A store
/// failure is logged but does not abort the publish: an in-memory
/// update with no flash record is preferable to a stale config.
async fn apply_update<S: Storage>(new: &Config, storage: &mut S) {
    persist(new, storage).await;
    STATE_WATCH.sender().send(new.clone());
    // Single writer: only the `run` task calls this, so read-then-send
    // cannot race another bump.
    let generation = GENERATION_WATCH.try_get().map_or(1, |g| g.wrapping_add(1));
    GENERATION_WATCH.sender().send(generation);
    info!("config: applied update, generation {}", generation);
    crate::events::config_changed(new.tuning.frequency_mhz, new.tuning.voltage_mv);
}

/// Persist a network-only change and publish it without halting mining.
///
/// Network settings (IP mode, hostname) are not on the hashing path, so
/// unlike [`apply_update`] this skips the stratum restart. The live
/// network apply flow has already switched the
/// stack to the new address by the time this runs; the stratum client
/// reconnects on its own when its socket drops, so all this needs to do
/// is persist the confirmed record and refresh [`STATE_WATCH`] so
/// `GET /api/config` reflects it.
async fn set_network<S: Storage>(config: &Config, storage: &mut S) {
    persist(config, storage).await;
    STATE_WATCH.sender().send(config.clone());
    info!("config: applied network update");
}

/// Reset the stored config to [`Config::default`] and publish it.
///
/// Persists the defaults, refreshes [`STATE_WATCH`], and returns the
/// new value. Returning the config keeps the persisted, published, and
/// in-memory values provably identical and lets tests drive the real
/// reset path.
async fn factory_reset<S: Storage>(storage: &mut S, board: &'static HashboardDescriptor) -> Config {
    let config = Config::for_board(board);
    persist(&config, storage).await;
    STATE_WATCH.sender().send(config.clone());
    info!("config: factory reset to defaults");
    crate::events::factory_reset();
    config
}

/// Persist `config` to `storage`. A failure is logged and swallowed:
/// an in-memory-only update is preferable to leaving mining paused.
async fn persist<S: Storage>(config: &Config, storage: &mut S) {
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let Ok(length) = encode_blob(config, &mut buffer) else {
        warn!("config: encode failed, publishing in-memory only");
        return;
    };
    if storage.store(CONFIG_KEY, &buffer[..length]).await.is_err() {
        warn!("config: store failed, publishing in-memory only");
    }
}
