//! Mining firmware configuration schema, distribution handle, and
//! the reconfigure path.
//!
//! Defines the [`Config`] type that carries all runtime-tunable
//! parameters and the [`Handle`] consumers use to read the current
//! config and wait for changes.
//!
//! Firmware spawns [`run`], which publishes [`Config::default`] once
//! at boot and then drains [`COMMAND_CHANNEL`] forever. On each
//! [`Message::Update`] it brackets the new value with
//! `Reconfiguring(true)` / `Reconfiguring(false)` on the manager,
//! waiting for mining to halt before publishing on [`STATE_WATCH`].

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

mod message;

#[cfg(test)]
#[path = "tests/storage.rs"]
mod tests_storage;

pub mod api;
pub mod cooling;
pub mod mining;
pub mod pool;
pub mod thermal;

pub use api::ApiConfig;
pub use cooling::{CoolingConfig, PidConfig};
pub use mining::TuningConfig;
pub use pool::{
    PoolEntry, ProtocolKind, DEFAULT_POOL_HOST, DEFAULT_POOL_PORT, DEFAULT_POOL_SV2_PUBKEY,
    DEFAULT_POOL_USER,
};
pub use thermal::ThermalConfig;

pub mod storage;
pub use storage::{decode_blob, encode_blob, LoadError, Storage, CONFIG_KEY};

pub mod flash_storage;
pub use flash_storage::FlashStorage;

pub mod sync_flash;
pub use sync_flash::SyncFlash;

#[cfg(feature = "std")]
pub mod file_storage;
#[cfg(feature = "std")]
pub use file_storage::FileStorage;

pub use message::Message;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use ubos_logging::{info, warn};
use ubos_manager::{Handle as ManagerHandle, Message as ManagerMessage};
use ubos_mining::Handle as MiningHandle;

/// Subscriber capacity for the config watch.
///
/// Covers thermal, cooling, mining/tuner, manager, API, and stratum
/// client, with two slots spare for future consumers.
const WATCH_CAPACITY: usize = 8;

/// Depth of the config command channel.
const COMMAND_CHANNEL_DEPTH: usize = 4;

/// Stack buffer size for encode and decode of stored config blobs.
///
/// [`flash_storage::SCRATCH_SIZE`] is a const alias for this value, so
/// raising it here raises the flash scratch budget in lockstep.
pub(crate) const LOAD_BUFFER_SIZE: usize = 512;

type CSMutex = CriticalSectionRawMutex;

/// Module-internal state watch carrying the live [`Config`]. Single
/// writer is [`run`].
pub(crate) static STATE_WATCH: Watch<CSMutex, Config, WATCH_CAPACITY> = Watch::new();

/// Module-internal command channel. Single consumer is [`run`];
/// producers send via [`Handle::send`].
pub(crate) static COMMAND_CHANNEL: Channel<CSMutex, Message, COMMAND_CHANNEL_DEPTH> =
    Channel::new();

/// Fires once each time a successful [`Message::Update`] has been
/// persisted and published. The stratum client awaits it in its
/// reconnect loop so a pool change triggers an immediate teardown
/// instead of waiting for the next session error.
pub(crate) static STRATUM_RESTART: Signal<CSMutex, ()> = Signal::new();

/// Per-task subscription returned by [`Handle::subscribe`].
pub type ConfigSubscription = Receiver<'static, CSMutex, Config, WATCH_CAPACITY>;

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

    /// Submit a config update. The reconfigure task running in [`run`]
    /// brackets the new value with `Reconfiguring(true)`/`(false)` on
    /// the manager, then publishes on `STATE_WATCH` once mining has
    /// halted.
    pub async fn send(&self, message: Message) {
        COMMAND_CHANNEL.send(message).await;
    }

    /// Signal fired after each successful config update has been
    /// persisted and published. Consumers that need to react before the
    /// next [`Handle::changed`] tick (the stratum client tearing down
    /// its session, for example) await this signal directly.
    #[must_use]
    pub fn stratum_restart(&self) -> &'static Signal<CSMutex, ()> {
        &STRATUM_RESTART
    }
}

/// Pool-only partial update accepted by `POST /api/config`.
///
/// The dashboard form edits pool credentials in isolation; sending a
/// full [`Config`] would force the UI to round-trip every unrelated
/// field (thermal, cooling, api) on every save. The handler merges
/// this struct into [`ConfigHandle::current`] before sending
/// [`Message::Update`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct PoolUpdate {
    /// Replacement value for [`Config::pool`].
    pub pool: heapless::Vec<PoolEntry, 3>,
}

/// All runtime-tunable firmware parameters.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, postcard_schema::Schema,
)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Config {
    /// Ordered list of pool endpoints; the client tries them in order.
    pub pool: heapless::Vec<PoolEntry, 3>,
    /// Thermal monitoring parameters.
    pub thermal: ThermalConfig,
    /// Cooling loop parameters.
    pub cooling: CoolingConfig,
    /// ASIC tuning parameters.
    pub tuning: TuningConfig,
    /// HTTP API credentials.
    pub api: ApiConfig,
}

impl Default for Config {
    /// Factory defaults matching today's hardcoded BMM101 profile.
    ///
    /// Not a `const` because [`ApiConfig`]'s default cannot be `const`
    /// (heapless 0.9 `String::push_str` is not `const fn`). The value
    /// is computed once at boot; it is cheap.
    fn default() -> Self {
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
        };
        let mut pool = heapless::Vec::new();
        pool.push(entry)
            .expect("BUG: single default pool entry exceeds Config::pool capacity");
        Self {
            pool,
            thermal: ThermalConfig::default(),
            cooling: CoolingConfig::default(),
            tuning: TuningConfig::default(),
            api: ApiConfig::default(),
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
/// Each [`Message::Update`] then runs the reconfigure bracket, persists
/// the new value before publishing it, and fires [`STRATUM_RESTART`] so
/// the stratum client tears down its session.
pub async fn run<S: Storage>(manager: ManagerHandle, mining: MiningHandle, mut storage: S) -> ! {
    let initial = load_initial(&mut storage).await;
    STATE_WATCH.sender().send(initial);
    info!("config: started");

    loop {
        let message = COMMAND_CHANNEL.receive().await;
        match message {
            Message::Update(new) => {
                apply_update(new, manager, mining, &mut storage).await;
            }
        }
    }
}

/// Load the persisted [`Config`] or fall back to [`Config::default`].
///
/// The buffer is sized to match [`encode_blob`]'s budget so any blob
/// that could have been stored fits without reallocation. All failure
/// paths converge on defaults so the firmware always boots with a
/// valid config.
async fn load_initial<S: Storage>(storage: &mut S) -> Config {
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let blob = match storage.load(CONFIG_KEY, &mut buffer).await {
        Ok(Some(blob)) => blob,
        Ok(None) => {
            info!("config: no stored config, using defaults");
            return Config::default();
        }
        Err(_) => {
            warn!("config: load failed, using defaults");
            return Config::default();
        }
    };
    match decode_blob::<S::Error>(blob) {
        Ok(config) => {
            info!("config: loaded from storage");
            config
        }
        Err(error) => {
            log_decode_error(&error);
            Config::default()
        }
    }
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
/// `Reconfiguring(true)` halts mining; we wait for the halt, persist
/// the new value, publish it on [`STATE_WATCH`], signal stratum to
/// reconnect, then clear `Reconfiguring`. A store failure is logged
/// but does not abort the publish: an in-memory update with no flash
/// record is preferable to leaving mining paused.
async fn apply_update<S: Storage>(
    new: Config,
    manager: ManagerHandle,
    mining: MiningHandle,
    storage: &mut S,
) {
    manager.send(ManagerMessage::Reconfiguring(true)).await;
    wait_until_mining_halted(mining).await;
    persist(&new, storage).await;
    STATE_WATCH.sender().send(new);
    STRATUM_RESTART.signal(());
    info!("config: applied update");
    manager.send(ManagerMessage::Reconfiguring(false)).await;
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

/// Wait until the mining subsystem reports `running == false`.
///
/// Checks the current value first so no await is needed when mining is
/// already halted. A fresh `Watch::receiver()` is initialised "unread",
/// so if `running` flips to `false` between the `state()` check and the
/// `subscribe()` call, the first `changed().await` resolves with the
/// already-published value rather than waiting for the next edge.
async fn wait_until_mining_halted(mining: MiningHandle) {
    if mining.state().is_some_and(|state| !state.running) {
        return;
    }
    let mut subscription = mining.subscribe();
    loop {
        let state = subscription.changed().await;
        if !state.running {
            return;
        }
    }
}
