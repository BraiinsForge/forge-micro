// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! Mining firmware configuration schema and distribution handle.
//!
//! Defines the [`Config`] type that carries all runtime-tunable
//! parameters and the [`Handle`] consumers use to read the current
//! config and wait for changes.
//!
//! Firmware spawns [`run`], which publishes one config at boot: the
//! stored one, or on a first boot [`Config::for_board`], with each field
//! of the board's [`Seed`] written over it whose source changed since
//! the store last took it. It then drains the command channel forever.
//! On each [`Message::Update`] it persists the new value and publishes
//! it to every [`Handle`].

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

// The host test binary links std for the `rusty-fork` macro.
#[cfg(test)]
extern crate std;

mod message;
mod versions;

#[cfg(test)]
#[path = "tests/fixture.rs"]
mod tests_fixture;

#[cfg(test)]
#[path = "tests/storage.rs"]
mod tests_storage;

#[cfg(test)]
#[path = "tests/versions.rs"]
mod tests_versions;

#[cfg(test)]
#[path = "tests/validation.rs"]
mod tests_validation;

#[cfg(test)]
#[path = "tests/handle.rs"]
mod tests_handle;

#[cfg(all(test, feature = "std"))]
#[path = "tests/factory_reset.rs"]
mod tests_factory_reset;

#[cfg(all(test, feature = "std"))]
#[path = "tests/seed.rs"]
mod tests_seed;

#[cfg(all(test, feature = "schema"))]
#[path = "tests/schema.rs"]
mod tests_schema;

pub mod api;
pub mod board;
pub mod cooling;
pub mod fee;
pub mod miner;
pub mod mining;
pub mod network;
pub mod pool;
pub mod pubkey;
pub mod seed;
pub mod thermal;

use seed::Taken;
use storage::SEED_KEY;
use ubos_core::units::{Efficiency, Frequency, Power, Voltage};

pub use api::ApiConfig;
pub use board::HashboardDefaults;
pub use cooling::{
    BOUNDS as COOLING_BOUNDS, CoolingBounds, CoolingConfig, CoolingInfo, FanMode,
    INFO as COOLING_INFO, PidConfig, PidTarget, Statistic,
};
pub use fee::{
    FEE_DEN, FeePool, FeeSlot, FeeWorker, Fixed, MAX_FEE_PERMILLE, MIN_FEE_SLOTS, REPAY_SPACING,
    UNREGISTERED,
};
pub use miner::{MinerConfig, Tuning};
pub use mining::{
    DefaultTuning, FrequencyVoltageTarget, MiningBounds, MiningInfo, PowerTarget, TunerInfo,
    bounds, mining_info,
};
pub use network::{NetworkConfig, NetworkMode};
pub use pool::{
    DEFAULT_POOL_HOST, DEFAULT_POOL_PORT, DEFAULT_POOL_SV2_PUBKEY, DEFAULT_POOL_USER, DEVFEE,
    MAX_POOL_WEIGHT, NAME_CAPACITY, POOL_SLOTS, PoolEntry, ProtocolKind,
};
pub use seed::{Seed, SeedField};
pub use thermal::{
    BOUNDS as THERMAL_BOUNDS, INFO as THERMAL_INFO, Source, TemperatureDescriptor, ThermalBounds,
    ThermalConfig, ThermalInfo,
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
pub use versions::live::Config;
pub use versions::{MIN_SCHEMA_PREFIX_LEN, SchemaPrefix, StagedConfig};

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use portable_atomic::{AtomicBool, AtomicU64, Ordering};
use ubos_logging::{info, warn};

ubos_events::events! {
    tuning_changed_fixed(frequency: Frequency, voltage: Voltage) info = "Configuration changed: {} MHz, {} V",
    tuning_changed_power(power: Power, efficiency: Efficiency)  info = "Configuration changed: {} W, {} J/TH",
    factory_reset()                                      warn = "Factory reset to defaults",
    network_applied()                                    info = "Network config applied",
    network_reverted()                                   warn = "Network config reverted",
    config_upgraded(from: u16, to: u16)                  info = "Configuration upgraded from version {} to version {}",
    config_upgrade_failed(from: u16, to: u16)            warn = "Cannot upgrade the configuration from version {} to version {}, defaults in use",
    config_version_unknown()                             warn = "Stored configuration has an unknown version, defaults in use",
    config_unusable()                                    warn = "Cannot use the stored configuration, defaults in use",
    config_tuning_adjusted()                             warn = "Stored tuning does not fit this firmware and is adjusted",
    config_downgraded(from: u16, to: u16)                info = "Configuration converted from version {} to version {} for the update",
    config_downgrade_failed(from: u16, to: u16)           warn = "Cannot convert the configuration from version {} to version {} for the update",
}

/// Subscriber capacity for the config watch, with spare.
const WATCH_SUBSCRIBERS: usize = 8;

/// Receiver slots that tasks hold while they wait in [`Handle::current`]
/// for the first publish. It covers every task that can wait: cooling,
/// thermal, net control, the pool-weights watcher, one stratum session for
/// each pool slot, and the serve task, with spare slots.
const FIRST_PUBLISH_WAITERS: usize = 32;

/// Receiver slots of the config watch: the subscribers and the waiters.
const WATCH_CAPACITY: usize = WATCH_SUBSCRIBERS + FIRST_PUBLISH_WAITERS;

const COMMAND_CHANNEL_DEPTH: usize = 4;

/// Buffer for the widest record: the schema-hash prefix plus the encoded
/// [`Config`] with every field at its widest, pinned by
/// `widest_record_fills_the_buffer`. The flash scratch aliases it.
pub(crate) const LOAD_BUFFER_SIZE: usize = 1408;

/// Stack buffer size for encode and decode of the seed record, which
/// holds a fingerprint for each [`Seed`] field.
///
/// The config task holds this buffer at its deepest await, beside the
/// boot config and the seed, so it is sized for the seed record rather
/// than for a config record. The test
/// `widest_seed_record_fills_its_buffer` pins it to the widest record.
pub(crate) const TAKEN_BUFFER_SIZE: usize = 18;

type CSMutex = CriticalSectionRawMutex;

/// Watch carrying the committed [`Config`], never the unconfirmed
/// network preview, so an update built from `current()` cannot carry a
/// preview into flash.
type ConfigWatch = Watch<CSMutex, Config, WATCH_CAPACITY>;

static STATE_WATCH: ConfigWatch = Watch::new();

/// `ubos-net` is the one subscriber, plus a spare.
const NETWORK_WATCH_CAPACITY: usize = 2;

/// Watch carrying the network the live stack should hold: the preview
/// while a confirm window is armed, the committed network otherwise.
/// Separate from `STATE_WATCH` so the preview reaches the stack without
/// reaching flash. Published on transitions only, never at boot.
type NetworkWatch = Watch<CSMutex, NetworkConfig, NETWORK_WATCH_CAPACITY>;

static NETWORK_WATCH: NetworkWatch = Watch::new();

type ConfigCommandChannel = Channel<CSMutex, Message, COMMAND_CHANNEL_DEPTH>;

static COMMAND_CHANNEL: ConfigCommandChannel = Channel::new();

/// One per session slot, one for the weights watcher, one spare.
const GENERATION_WATCH_CAPACITY: usize = pool::POOL_SLOTS + 2;

/// Bumped after each persisted [`Message::Update`], so every stratum
/// session tears down at once instead of on its next error. A `Watch`,
/// not a `Signal`, because every session task must see every bump.
type GenerationWatch = Watch<CSMutex, u32, GENERATION_WATCH_CAPACITY>;

static GENERATION_WATCH: GenerationWatch = Watch::new();

/// How long [`Handle::downgrade`] waits for the config task to store the
/// record of the staged image. A config task that does not answer must
/// not keep the board from the reboot, with mining already stopped.
const DOWNGRADE_TIMEOUT: Duration = Duration::from_secs(5);

/// The answer [`run`] signals once it has downgraded the config. It
/// serves one waiter, which the OTA handler is: the handler calls
/// [`Handle::downgrade`] under the OTA target lock, which admits one
/// upload at a time.
static DOWNGRADE_DONE: Signal<CSMutex, ()> = Signal::new();

/// How long a network preview holds before it reverts. A wrong static
/// address strands the board, so the operator must confirm from the new
/// address.
const CONFIRM_WINDOW: Duration = Duration::from_secs(120);

/// The pending network preview. Atomics, because the API handlers read
/// it without awaiting the config task.
struct PendingStatus {
    /// Uptime-seconds deadline of the preview, or 0.
    deadline_secs: AtomicU64,
    /// The preview is static; meaningful only while one is armed.
    is_static: AtomicBool,
}

impl PendingStatus {
    const fn new() -> Self {
        Self {
            deadline_secs: AtomicU64::new(0),
            is_static: AtomicBool::new(false),
        }
    }
}

static PENDING_STATUS: PendingStatus = PendingStatus::new();

pub type ConfigSubscription = Receiver<'static, CSMutex, Config, WATCH_CAPACITY>;

pub type NetworkSubscription = Receiver<'static, CSMutex, NetworkConfig, NETWORK_WATCH_CAPACITY>;

pub type GenerationSubscription = Receiver<'static, CSMutex, u32, GENERATION_WATCH_CAPACITY>;

static TAKEN: ubos_core::take_once::TakeOnce = ubos_core::take_once::TakeOnce::new("config");

/// Consumer-side handle. It holds no data: it proves the firmware took
/// it, and a copy goes to each task that reads the configuration, so a
/// task's arguments show what it talks to.
#[derive(Clone, Copy)]
pub struct Handle(());

/// The right to run the task that stores and publishes the configuration.
///
/// It is not `Copy`, so a firmware that gives it to one task cannot give
/// it to a second.
pub struct Runner(());

#[allow(clippy::unused_self)]
impl Runner {
    pub(crate) fn publish(&self, config: Config) {
        STATE_WATCH.sender().send(config);
    }

    pub(crate) async fn next_message(&self) -> Message {
        COMMAND_CHANNEL.receive().await
    }
}

impl Handle {
    /// The firmware's one config handle, and the runner of the task that
    /// stores and publishes the configuration. A second call panics.
    #[must_use]
    #[track_caller]
    pub fn take() -> (Runner, Self) {
        TAKEN.take();
        (Runner(()), Self(()))
    }

    /// Returns the most recently published config, and waits for the
    /// first publish when the config task has made none yet.
    ///
    /// A task that starts before the config task has read the store
    /// waits here, so it never runs on values that the board did not
    /// state. The wait holds a receiver of the watch until the publish
    /// and gives it back on return.
    ///
    /// # Panics
    ///
    /// Panics if more tasks wait than [`FIRST_PUBLISH_WAITERS`] allows.
    pub async fn current(&self) -> Config {
        if let Some(config) = STATE_WATCH.try_get() {
            return config;
        }
        STATE_WATCH
            .receiver()
            .expect("BUG: first-publish waiters exhausted the receiver slots; raise FIRST_PUBLISH_WAITERS")
            .get()
            .await
    }

    /// The entry a session slot runs: the fee pool's at the fee slot, the
    /// operator's elsewhere. Owned, because the fee entry is built on each
    /// read, and its worker name follows the operator pools the board runs.
    pub async fn pool_slot(&self, slot: usize) -> Option<PoolEntry> {
        let current = self.current().await;
        if let Some(fee) = fee::FEE_SLOT.try_get()
            && fee.slot == slot
        {
            let operator = &current.pool[..fee.slot.min(current.pool.len())];
            return Some(fee.pool.entry(operator));
        }
        current.pool.get(slot).cloned()
    }

    /// The `GET /api/config` body: the current config with the fee entry
    /// and its share beside the operator's list.
    pub async fn view(&self) -> ConfigUpdate {
        let current = self.current().await;
        let mut view = ConfigUpdate::from_config(&current);
        if let Some(fee) = fee::FEE_SLOT.try_get() {
            let operator = &current.pool[..fee.slot.min(current.pool.len())];
            view.fee = Some(fee.pool.entry(operator));
            view.fee_permille = Some(fee.pool.permille);
        }
        view
    }

    /// One per consumer task.
    ///
    /// # Panics
    ///
    /// When the `Watch` has no free receiver slot.
    #[must_use]
    pub fn subscribe(&self) -> ConfigSubscription {
        STATE_WATCH
            .receiver()
            .expect("BUG: Handle receiver slots exhausted; raise WATCH_CAPACITY")
    }

    pub async fn changed(&self) -> Config {
        let mut subscription = self.subscribe();
        subscription.changed().await
    }

    /// Subscription to the effective-network watch, which publishes on
    /// transitions only, so the first `changed()` blocks until one.
    ///
    /// # Panics
    ///
    /// When the `Watch` has no free receiver slot.
    #[must_use]
    pub fn subscribe_network(&self) -> NetworkSubscription {
        NETWORK_WATCH
            .receiver()
            .expect("BUG: NETWORK_WATCH receiver slots exhausted; raise NETWORK_WATCH_CAPACITY")
    }

    pub async fn send(&self, message: Message) {
        COMMAND_CHANNEL.send(message).await;
    }

    /// `false` when the channel is full, so an HTTP handler can answer
    /// busy instead of blocking.
    #[must_use]
    pub fn try_send(&self, message: Message) -> bool {
        COMMAND_CHANNEL.try_send(message).is_ok()
    }

    /// Downgrade the config for the image that an OTA staged, and wait
    /// until the config task has stored the record that the image reads.
    /// Returns `false` when the task is busy or does not answer within
    /// [`DOWNGRADE_TIMEOUT`].
    ///
    /// The task stores nothing after the downgrade, so the caller reboots
    /// into the image next. The message goes out without an await,
    /// because a `Message` is as large as a `Config` and an awaited send
    /// would hold one in the future of the caller.
    pub async fn downgrade(&self, staged: StagedConfig) -> bool {
        DOWNGRADE_DONE.reset();
        if !self.try_send(Message::Downgrade(staged)) {
            return false;
        }
        with_timeout(DOWNGRADE_TIMEOUT, DOWNGRADE_DONE.wait())
            .await
            .is_ok()
    }

    /// Seconds left in the network confirm window, or 0.
    #[must_use]
    pub fn pending_seconds_remaining(&self) -> u64 {
        let deadline = PENDING_STATUS.deadline_secs.load(Ordering::Relaxed);
        if deadline == 0 {
            return 0;
        }
        deadline.saturating_sub(Instant::now().as_secs())
    }

    /// `Some(true)` for a pending static preview, `Some(false)` for DHCP,
    /// `None` when nothing is pending.
    #[must_use]
    pub fn pending_mode_is_static(&self) -> Option<bool> {
        if self.pending_seconds_remaining() == 0 {
            return None;
        }
        Some(PENDING_STATUS.is_static.load(Ordering::Relaxed))
    }

    /// Subscription to the update generation; see [`GenerationWatch`].
    ///
    /// # Panics
    ///
    /// When the `Watch` has no free receiver slot.
    #[must_use]
    pub fn subscribe_generation(&self) -> GenerationSubscription {
        GENERATION_WATCH.receiver().expect(
            "BUG: GENERATION_WATCH receiver slots exhausted; raise GENERATION_WATCH_CAPACITY",
        )
    }
}

/// Partial-update cooling section of the config update.
///
/// Only the operator-tunable cooling fields are on the wire; the PID
/// gains are factory-set and never patched. Each key is optional so one
/// can be set without the others. `mode` and `fixed_fan_pwm` are separate
/// keys rather than one payload variant so the UI can edit the duty
/// without leaving auto control.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CoolingUpdate {
    /// The fan mode; a missing key leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FanMode>,
    /// The reading the auto-mode PID follows and the temperature it holds;
    /// a missing key leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PidTarget>,
    /// The duty held in fixed mode, 0 to 100; a missing key leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_fan_pwm: Option<u8>,
    /// The fan floor in auto mode, 0 to 100; a missing key leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_fan_pwm: Option<u8>,
}

/// Partial-update wire shape for `GET` and `POST /api/config`.
///
/// Every key is optional, so a caller can patch one section at a time
/// without round-tripping unrelated state. A missing key leaves that
/// section unchanged; the POST handler merges only the keys present into
/// the running config. A GET response carries every section with
/// `api.password` blanked, so the stored secret never leaves the device.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConfigUpdate {
    /// The pool slots; a missing key leaves them unchanged. An empty array
    /// means no pools configured and clears every persisted slot, which is
    /// what the web UI sends when every URL input is blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schema", schemars(with = "Option<&[PoolEntry]>", length(max = pool::MAX_POOLS)))]
    pub pool: Option<heapless::Vec<PoolEntry, { pool::MAX_POOLS }>>,
    /// The entry the fee slot runs, which the board states and the
    /// operator cannot edit. `GET` carries it beside the operator's
    /// list, and leaves it out on a board with no fee slot. A `POST`
    /// carries it back as served and the field is not even parsed, so
    /// a `POST` of the `GET` body is an identity whatever the entry
    /// holds.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub fee: Option<PoolEntry>,
    /// The share of the work the fee slot takes, in thousandths, which
    /// the board states with the entry. `GET` carries it beside `fee`
    /// and leaves it out with it, and a `POST` carries it back as
    /// served and the field is not parsed, as `fee` is not.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub fee_permille: Option<i64>,
    /// The thermal section; a missing key leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal: Option<ThermalConfig>,
    /// Partial cooling update: mode, setpoint, fixed duty and fan floor.
    /// A missing key leaves the section unchanged. The PID gains are
    /// factory-tuned and are not on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooling: Option<CoolingUpdate>,
    /// The miner section; a missing key leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miner: Option<MinerConfig>,
    /// The API section; a missing key leaves it unchanged.
    ///
    /// On `GET`, `password` is an empty string so the stored secret never
    /// leaves the device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ApiConfig>,
    /// The network section; a missing key leaves it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkConfig>,
}

impl ConfigUpdate {
    /// Merge the `Some` fields into `target`.
    pub fn apply(self, target: &mut Config) {
        if let Some(pool) = self.pool {
            target.pool = pool;
        }
        if let Some(thermal) = self.thermal {
            target.thermal = thermal;
        }
        if let Some(cooling) = self.cooling {
            if let Some(mode) = cooling.mode {
                target.cooling.mode = mode;
            }
            if let Some(pid_target) = cooling.target {
                target.cooling.target = pid_target;
            }
            if let Some(pwm) = cooling.fixed_fan_pwm {
                target.cooling.fixed_fan_pwm = pwm;
            }
            if let Some(pwm) = cooling.min_fan_pwm {
                target.cooling.pid.min_fan_pwm = pwm;
            }
        }
        if let Some(miner) = self.miner {
            target.miner = miner;
        }
        if let Some(api) = self.api {
            target.api = api;
        }
        if let Some(network) = self.network {
            target.network = network;
        }
    }

    /// The base of the `GET /api/config` body, with the password blanked.
    /// A `Config` holds no fee slot, so [`Handle::view`] fills `fee`.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let mut api = config.api.clone();
        api.password = heapless::String::new();
        Self {
            pool: Some(config.pool.clone()),
            fee: None,
            fee_permille: None,
            thermal: Some(config.thermal.clone()),
            cooling: Some(CoolingUpdate {
                mode: Some(config.cooling.mode),
                target: Some(config.cooling.target),
                fixed_fan_pwm: Some(config.cooling.fixed_fan_pwm),
                min_fan_pwm: Some(config.cooling.pid.min_fan_pwm),
            }),
            miner: Some(config.miner.clone()),
            api: Some(api),
            network: Some(config.network.clone()),
        }
    }

    /// Validate the present fields against `bounds` and against `current`,
    /// the config they merge into. An incoming `thermal` must name the
    /// reading kinds of `current`, because they belong to the machine. The
    /// cooling target must name a kind the machine reads and stay below its
    /// dangerous temperature, checked whichever side the update carries,
    /// against the incoming value where present.
    ///
    /// # Errors
    ///
    /// The first failing rule, as an operator-readable string.
    pub fn validate(&self, current: &Config, bounds: &MiningBounds) -> Result<(), &'static str> {
        if let Some(pool) = &self.pool {
            for (slot, entry) in pool.iter().enumerate() {
                entry.validate(slot)?;
            }
        }
        if let Some(thermal) = &self.thermal {
            thermal.validate()?;
            if !thermal.dangerous.same_kinds(&current.thermal.dangerous) {
                return Err(
                    "thermal must set a dangerous temperature for each reading kind of the machine, and for no other kind",
                );
            }
        }
        if let Some(miner) = &self.miner {
            miner.validate(bounds)?;
        }
        if let Some(api) = &self.api {
            api.validate()?;
        }
        if let Some(network) = &self.network {
            network.validate()?;
        }
        let cooling_target = self.cooling.as_ref().and_then(|cooling| cooling.target);
        if let Some(pwm) = self
            .cooling
            .as_ref()
            .and_then(|cooling| cooling.min_fan_pwm)
        {
            CoolingConfig::validate_min_fan_pwm(pwm)?;
        }
        if let Some(pwm) = self
            .cooling
            .as_ref()
            .and_then(|cooling| cooling.fixed_fan_pwm)
        {
            CoolingConfig::validate_fixed_fan_pwm(pwm)?;
        }

        if self.thermal.is_some() || cooling_target.is_some() {
            let dangerous = self
                .thermal
                .as_ref()
                .map_or(current.thermal.dangerous, |thermal| thermal.dangerous);

            if let Some(target) = cooling_target {
                CoolingConfig::validate_target(target, dangerous)?;
            } else if CoolingConfig::validate_target(current.cooling.target, dangerous).is_err() {
                return Err(
                    "current cooling.target.temp must remain less than the dangerous temperature of its kind",
                );
            }
        }

        Ok(())
    }
}

impl Config {
    /// Factory defaults for a board that hands in `defaults`, with
    /// `default_tuning` as the tuning.
    ///
    /// Not a `const fn` because [`ApiConfig`]'s default cannot be `const`
    /// (heapless 0.9 `String::push_str` is not `const fn`). The value is
    /// computed once at boot; it is cheap.
    #[must_use]
    pub fn for_board(defaults: &HashboardDefaults, default_tuning: DefaultTuning) -> Self {
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
            protocol: pool::default_protocol(option_env!("POOL_PROTOCOL")),
            weight: 0,
        };
        let mut pool = heapless::Vec::new();
        pool.push(entry)
            .expect("BUG: single default pool entry exceeds Config::pool capacity");
        Self {
            pool,
            thermal: ThermalConfig {
                // The halt thresholds come from the board rather than from
                // a single hardcoded default, so a board comes up with a
                // threshold for each kind of reading it measures: its chip
                // family's junction ceiling, the temperature its hashboard
                // may reach, or both.
                dangerous: defaults.dangerous,
                sample_interval_ms: thermal::DEFAULT_SAMPLE_INTERVAL_MS,
                cooldown_temp: thermal::DEFAULT_COOLDOWN_TEMP,
            },
            cooling: CoolingConfig {
                // A fixed duty is an operator choice, never a default.
                mode: FanMode::Auto,
                // The board states its own setpoint. It is editable at
                // run time through the UI afterwards.
                target: defaults.cooling_target,
                fixed_fan_pwm: cooling::DEFAULT_FIXED_FAN_PWM,
                pid: PidConfig {
                    min_fan_pwm: defaults.min_fan_pwm.percent(),
                    input_smoother_half_decay_secs: defaults.input_smoother_half_decay_secs,
                    ..PidConfig::default()
                },
            },
            miner: MinerConfig {
                tuning: default_tuning.tuning(defaults),
            },
            api: ApiConfig::default(),
            network: NetworkConfig::default(),
        }
    }
}

impl Config {
    /// Schema fingerprint, FNV1a-64 over the postcard schema. Any field
    /// change shifts it, and the boot reads a record only under this hash
    /// or the hash of a version it upgrades from, so a firmware update
    /// falls back to defaults instead of misreading flash.
    pub const SCHEMA_HASH: [u8; 8] = versions::schema_hash::<Config>();

    /// Validate operator-tunable fields against their per-domain bounds.
    ///
    /// Runs the pool, thermal, miner, api, and network
    /// sub-validators, bounds both cooling duties, and enforces the
    /// cross-field rule that `cooling.target` names a kind of reading in
    /// `thermal` and stays below its dangerous temperature.
    /// Stored blobs pass through serde and can carry out-of-range or
    /// non-finite floats in those fields; [`load_initial`] calls this after
    /// [`decode_blob`] so a corrupted blob falls back to defaults instead of
    /// feeding consumers values that bypass per-update validation.
    ///
    /// `min_fan_pwm` sits under `cooling.pid` but is operator-tunable through
    /// [`CoolingUpdate::min_fan_pwm`] and so reaches flash from the UI; it is
    /// bounded here alongside `cooling.fixed_fan_pwm`. The remaining
    /// `cooling.pid` gains have no [`ConfigUpdate`] path and enter flash only
    /// via [`Config::for_board`]; they are not re-validated here.
    ///
    /// # Errors
    ///
    /// The first failing rule, as an operator-readable string.
    pub fn validate(&self, bounds: &MiningBounds) -> Result<(), &'static str> {
        for (slot, entry) in self.pool.iter().enumerate() {
            entry.validate(slot)?;
        }
        self.thermal.validate()?;
        self.miner.validate(bounds)?;
        self.api.validate()?;
        self.network.validate()?;
        CoolingConfig::validate_target(self.cooling.target, self.thermal.dangerous)?;
        CoolingConfig::validate_fixed_fan_pwm(self.cooling.fixed_fan_pwm)?;
        CoolingConfig::validate_min_fan_pwm(self.cooling.pid.min_fan_pwm)?;
        Ok(())
    }
}

/// The config task. A record an older version stored is upgraded and
/// stored again in the live schema. A `seed` field is written over the
/// config when its fingerprint differs from the one the store last took,
/// so a setting the operator has since changed here stays as they left
/// it. The seed is borrowed, because an async fn that takes it by value
/// holds it twice. `fee` is the slot from [`FeeSlot::last_of`] the handle
/// answers for. A network preview is published on the network watch and
/// never persisted, so a reboot mid-window comes up on the committed
/// config, and `committed.network` stays the revert target while a
/// preview is pending. After a [`Message::Downgrade`] the task drops
/// every command until the reboot, so no write replaces the record the
/// staged image reads.
pub async fn run<S: Storage>(
    runner: &mut Runner,
    mut storage: S,
    defaults: &HashboardDefaults,
    info: &'static MiningInfo,
    seed: Option<&Seed>,
    fee: Option<FeeSlot>,
) -> ! {
    // Before the first publish, which every reader of the fee slot
    // waits for, so none of them can read an empty slot and park.
    if let Some(fee) = fee {
        fee::install(fee);
    }
    let mut committed = load_initial(&mut storage, defaults, info, seed).await;
    runner.publish(committed.clone());
    let mut preview: Option<(NetworkConfig, Instant)> = None;
    info!("config: started");

    loop {
        let message = if let Some(&(_, deadline)) = preview.as_ref() {
            match select(runner.next_message(), Timer::at(deadline)).await {
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
            runner.next_message().await
        };

        match message {
            Message::Update(new) => {
                apply_update(runner, &new, &mut storage).await;
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
                    set_network(runner, &committed, &mut storage).await;
                    NETWORK_WATCH.sender().send(committed.network.clone());
                    crate::events::network_applied();
                }
            }
            Message::ConfirmNetwork => {
                if let Some((network, _)) = preview.take() {
                    clear_pending_status();
                    committed.network = network;
                    // The stack already holds the preview.
                    set_network(runner, &committed, &mut storage).await;
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
                committed =
                    factory_reset(runner, &mut storage, defaults, info.default_tuning).await;
                NETWORK_WATCH.sender().send(committed.network.clone());
                ubos_core::reboot::request_reboot();
            }
            Message::Downgrade(staged) => {
                store_downgraded(&committed, staged, &mut storage).await;
                DOWNGRADE_DONE.signal(());
                drop_until_reboot(runner).await;
            }
        }
    }
}

/// Store the record that the image an OTA staged reads, when that is an
/// older version than this build's.
///
/// The conversion runs without an await, so the task holds one config
/// across each suspend point. Only the encoded record waits for the
/// store.
async fn store_downgraded<S: Storage>(config: &Config, staged: StagedConfig, storage: &mut S) {
    let Some(version) = versions::downgrade_version(staged, versions::VERSIONS) else {
        info!("config: the new image gets the stored config as it is");
        return;
    };
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let Some(length) = (version.downgrade)(config, &mut buffer) else {
        warn!(
            "config: cannot encode the config of version {} for the new image",
            version.number
        );
        crate::events::config_downgrade_failed(versions::NEWEST_VERSION, version.number);
        return;
    };
    if storage.store(CONFIG_KEY, &buffer[..length]).await.is_err() {
        warn!(
            "config: cannot store the config of version {} for the new image",
            version.number
        );
        crate::events::config_downgrade_failed(versions::NEWEST_VERSION, version.number);
        return;
    }
    info!(
        "config: stored the config of version {} for the new image",
        version.number
    );
    crate::events::config_downgraded(versions::NEWEST_VERSION, version.number);
}

/// Drop every command until the OTA handler reboots into the staged
/// image, so no write replaces the record that the image reads.
async fn drop_until_reboot(runner: &Runner) -> ! {
    loop {
        let _ = runner.next_message().await;
        warn!("config: an update is installing, the change is not saved");
    }
}

/// Task shell around [`run`], which drives the persisted config store.
/// Takes the board's storage backend, which bottoms out in its own flash
/// driver, the [`HashboardDefaults`] of its hashboard, and the firmware's
/// [`MiningInfo`],
/// whose bounds the store validates against and whose default tuning a
/// new config starts on. The task's `seed` argument holds the settings
/// the board recovered from the firmware it replaced, when it has any to
/// offer; a board with none spawns it with `None`.
#[macro_export]
macro_rules! config_task {
    ($storage:ty, $defaults:expr, $info:expr, $fee:expr) => {
        #[embassy_executor::task]
        async fn config_task(
            mut runner: ubos_config::Runner,
            storage: $storage,
            seed: Option<ubos_config::Seed>,
        ) -> ! {
            ubos_config::run(&mut runner, storage, $defaults, $info, seed.as_ref(), $fee).await
        }
    };
}

fn arm_pending_status(network: &NetworkConfig, deadline: Instant) {
    // The mode goes first, so a reader that sees the deadline sees the
    // matching mode.
    PENDING_STATUS.is_static.store(
        matches!(network.mode, NetworkMode::Static { .. }),
        Ordering::Relaxed,
    );
    // Rounded up, so the reported seconds stay positive until the timer
    // fires.
    PENDING_STATUS
        .deadline_secs
        .store(deadline.as_millis().div_ceil(1000), Ordering::Relaxed);
}

fn clear_pending_status() {
    PENDING_STATUS.deadline_secs.store(0, Ordering::Relaxed);
    PENDING_STATUS.is_static.store(false, Ordering::Relaxed);
}

/// The persisted [`Config`], or the defaults on any failure, with each
/// seed field whose fingerprint differs from the taken one written over
/// it. A key never written is a clean first boot and logs nothing. A
/// stored config this build cannot read takes every seed field and is
/// stored at once; an I/O error takes none, so a flash fault never writes.
async fn load_initial<S: Storage>(
    storage: &mut S,
    defaults: &HashboardDefaults,
    info: &'static MiningInfo,
    seed: Option<&Seed>,
) -> Config {
    // The loaded record is scoped to the load and its upgrade, so the task
    // does not hold it across the seed writes.
    let (stored, lost) = {
        // The buffer is scoped to the load, so the task does not hold it
        // across the upgrade's store. An empty store reads as the defaults,
        // so `None` is a record this build cannot read.
        let loaded = {
            let mut buffer = [0u8; LOAD_BUFFER_SIZE];
            match storage.load(CONFIG_KEY, &mut buffer).await {
                Ok(Some(blob)) => decode_stored::<S::Error>(blob, defaults, info),
                Ok(None) => {
                    info!("config: no stored config, using defaults");
                    Some(Loaded::Live(Config::for_board(
                        defaults,
                        info.default_tuning,
                    )))
                }
                Err(_) => {
                    warn!("config: load failed, using defaults");
                    crate::events::config_unusable();
                    return Config::for_board(defaults, info.default_tuning);
                }
            }
        };
        // An upgraded record is stored again in this build's schema, so a
        // later build reads it after it drops the version that wrote it.
        // The store borrows the config, so the task holds one copy of it.
        if let Some(Loaded::Upgraded(config)) = &loaded {
            persist(config, storage).await;
        }
        let lost = loaded.is_none();
        (loaded.map(Loaded::into_config), lost)
    };
    let Some(seed) = seed else {
        return stored.unwrap_or_else(|| Config::for_board(defaults, info.default_tuning));
    };
    // The boot config is scoped to the merge, so the task holds one config
    // across the writes below rather than two. The merge moves `stored`
    // whole and never borrows it, because a borrowed local keeps its room
    // in the task until its scope ends.
    let (config, taken, store) = {
        let base = stored.unwrap_or_else(|| Config::for_board(defaults, info.default_tuning));
        let Some(taken) = load_taken(storage).await else {
            return base;
        };
        let from = if lost { Taken::default() } else { taken };
        let Some((config, taken)) = seeded(&base, from, seed, info.bounds) else {
            return base;
        };
        // A lost record is replaced even by a seed that matches the
        // defaults, so the next boot reads the config directly.
        let store = lost || config != base;
        (config, taken, store)
    };
    // The fingerprints are saved only once the config is stored, so a power
    // cut or a failed write between the two takes the seed again on the
    // next boot rather than losing it.
    if store && !persist(&config, storage).await {
        return config;
    }
    persist_taken(&taken, storage).await;
    config
}

/// A config the boot loaded, in the live schema or upgraded to it.
///
/// The boot stores an upgraded record again, and only the decode knows
/// which path a record took, so it hands the answer on in the type.
enum Loaded {
    /// A config in the live schema: the defaults or a record of this build.
    Live(Config),
    /// A record that an older version stored, upgraded to the live schema.
    Upgraded(Config),
}

impl Loaded {
    /// The config in the live schema, whichever path it took, for the
    /// boot to merge the seed into once the upgrade is stored.
    fn into_config(self) -> Config {
        let (Self::Live(config) | Self::Upgraded(config)) = self;
        config
    }
}

/// The config in a stored `blob`, or `None` after a logged reason when
/// it does not decode or fails validation.
fn decode_stored<E>(
    blob: &[u8],
    defaults: &HashboardDefaults,
    info: &'static MiningInfo,
) -> Option<Loaded> {
    let mut loaded = decode_record::<E>(blob)?;
    let (Loaded::Live(config) | Loaded::Upgraded(config)) = &mut loaded;
    // Firmware updates narrow the tuners and ranges a stored tuning
    // passed. Only the tuning is replaced then, so the operator keeps
    // the rest of their config.
    let stored = config.miner.tuning;
    if info.bounds.tuners.iter().any(|tuner| tuner.reads(&stored)) {
        config.miner.tuning = stored.clamped(info.bounds);
        if config.miner.tuning != stored {
            warn!("config: stored tuning outside the accepted ranges, clamped into them");
            crate::events::config_tuning_adjusted();
        }
    } else {
        warn!(
            "config: stored tuning selects a tuner this machine does not offer, using the default tuning"
        );
        crate::events::config_tuning_adjusted();
        config.miner.tuning = info.default_tuning.tuning(defaults);
    }
    // A firmware update can change the kinds of reading a machine
    // measures, and a threshold is kept only for a kind the machine reads.
    // Only the thresholds are replaced then, as the tuning is.
    let dangerous = defaults.dangerous;
    if !config.thermal.dangerous.same_kinds(&dangerous) {
        warn!(
            "config: stored dangerous temperatures name other reading kinds than the machine, using the default ones"
        );
        config.thermal.dangerous = dangerous;
    }
    // The target follows one kind, so a target of a kind the machine does
    // not read is replaced with the board's, as the thresholds are.
    if config
        .thermal
        .dangerous
        .temp(config.cooling.target.source)
        .is_none()
    {
        warn!(
            "config: stored cooling target follows a reading kind the machine does not measure, using the default target"
        );
        config.cooling.target = defaults.cooling_target;
    }
    if config.validate(info.bounds).is_err() {
        warn!("config: stored config failed validation, using defaults");
        crate::events::config_unusable();
        return None;
    }
    info!("config: loaded from storage");
    Some(loaded)
}

/// The live config in a stored record, or `None` after a logged reason
/// when it does not decode: decoded directly when the record is in this
/// build's schema, and upgraded when an older version wrote it.
fn decode_record<E>(blob: &[u8]) -> Option<Loaded> {
    let error = match decode_blob::<E>(blob) {
        Ok(config) => return Some(Loaded::Live(config)),
        Err(error) => error,
    };
    if let LoadError::SchemaMismatch { stored, .. } = error
        && let Some(version) = versions::older_version(stored, versions::VERSIONS)
    {
        return upgrade_record(version, &blob[stored.len()..]).map(Loaded::Upgraded);
    }
    log_decode_error(&error);
    None
}

/// The live config in the `payload` of a record of `version`, or `None`
/// after a logged reason when the payload does not decode.
///
/// The upgrade decodes and converts without an await, so the task never
/// holds the old and the new config across a suspend point.
fn upgrade_record(version: &versions::Version, payload: &[u8]) -> Option<Config> {
    let Some(config) = (version.upgrade)(payload) else {
        warn!(
            "config: the record of version {} does not decode, using defaults",
            version.number
        );
        crate::events::config_upgrade_failed(version.number, versions::NEWEST_VERSION);
        return None;
    };
    info!("config: upgraded the record of version {}", version.number);
    crate::events::config_upgraded(version.number, versions::NEWEST_VERSION);
    Some(config)
}

/// `base` with each field of the board's `seed` written over it whose
/// fingerprint differs from the one in `taken`, and the fingerprints
/// to save with it.
///
/// `None` when the seed has no new field, or when its result fails
/// validation. A rejected seed leaves its fingerprints unsaved, so the
/// next boot tries it again.
fn seeded(
    base: &Config,
    taken: Taken,
    seed: &Seed,
    bounds: &MiningBounds,
) -> Option<(Config, Taken)> {
    let mut config = base.clone();
    let mut now_taken = taken;
    seed.apply(&mut config, &mut now_taken);
    if now_taken == taken {
        return None;
    }
    if config.validate(bounds).is_err() {
        warn!("config: the config with the seed is not valid, seed not used");
        return None;
    }
    Some((config, now_taken))
}

/// The seed fingerprints the store last took, or `None` when the
/// record cannot be read, so a flash fault never writes the seed over
/// the config.
///
/// An absent record takes nothing, so every seed field is new. So is
/// every field of a record that does not decode, because the record is
/// written again only when a seed is taken.
async fn load_taken<S: Storage>(storage: &mut S) -> Option<Taken> {
    let mut buffer = [0u8; TAKEN_BUFFER_SIZE];
    let Ok(record) = storage.load(SEED_KEY, &mut buffer).await else {
        warn!("config: cannot read the seed record, seed not used");
        return None;
    };
    let Some(record) = record else {
        return Some(Taken::default());
    };
    Some(postcard::from_bytes(record).unwrap_or_else(|_| {
        warn!("config: seed record is damaged, all seed fields apply again");
        Taken::default()
    }))
}

/// Persist the seed fingerprints the store took. A failure is logged
/// and swallowed, and the next boot takes the same fields again.
async fn persist_taken<S: Storage>(taken: &Taken, storage: &mut S) {
    let mut buffer = [0u8; TAKEN_BUFFER_SIZE];
    let Ok(record) = postcard::to_slice(taken, &mut buffer) else {
        warn!("config: cannot encode the seed record");
        return;
    };
    if storage.store(SEED_KEY, record).await.is_err() {
        warn!("config: cannot store the seed record");
    }
}

/// Log why a stored blob could not be decoded, and record an event for a
/// schema bump alone: an update caused it, and a damaged or an unreadable
/// record is the same loss to the operator.
#[allow(clippy::match_same_arms)]
fn log_decode_error<E>(error: &LoadError<E>) {
    match error {
        LoadError::SchemaMismatch { .. } => {
            warn!(
                "config: the stored config has a schema that this firmware cannot read, using defaults"
            );
            crate::events::config_version_unknown();
        }
        LoadError::Corrupt => {
            warn!("config: stored blob corrupt, using defaults");
            crate::events::config_unusable();
        }
        LoadError::Io(_) => {
            warn!("config: storage error decoding blob, using defaults");
            crate::events::config_unusable();
        }
    }
}

/// Persist, publish and bump the generation.
async fn apply_update<S: Storage>(runner: &Runner, new: &Config, storage: &mut S) {
    persist(new, storage).await;
    runner.publish(new.clone());
    let generation = GENERATION_WATCH.try_get().map_or(1, |g| g.wrapping_add(1));
    GENERATION_WATCH.sender().send(generation);
    info!("config: applied update, generation {}", generation);
    match &new.miner.tuning {
        Tuning::Fixed(target) => {
            crate::events::tuning_changed_fixed(target.frequency, target.voltage);
        }
        Tuning::Power(target) => {
            crate::events::tuning_changed_power(target.input_power, target.efficiency);
        }
    }
}

/// Persist and publish a network change without the generation bump: the
/// stratum client reconnects on its own when the socket drops.
async fn set_network<S: Storage>(runner: &Runner, config: &Config, storage: &mut S) {
    persist(config, storage).await;
    runner.publish(config.clone());
    info!("config: applied network update");
}

/// Persist and publish the defaults, and return them so a test drives
/// the real path.
async fn factory_reset<S: Storage>(
    runner: &Runner,
    storage: &mut S,
    defaults: &HashboardDefaults,
    default_tuning: DefaultTuning,
) -> Config {
    let config = Config::for_board(defaults, default_tuning);
    persist(&config, storage).await;
    runner.publish(config.clone());
    info!("config: factory reset to defaults");
    crate::events::factory_reset();
    config
}

/// Persist `config` and say whether it reached the store. A failure is
/// logged and swallowed: an in-memory update beats leaving mining paused.
async fn persist<S: Storage>(config: &Config, storage: &mut S) -> bool {
    let mut buffer = [0u8; LOAD_BUFFER_SIZE];
    let Ok(length) = encode_blob(config, &mut buffer) else {
        warn!("config: encode failed, publishing in-memory only");
        return false;
    };
    if storage.store(CONFIG_KEY, &buffer[..length]).await.is_err() {
        warn!("config: store failed, publishing in-memory only");
        return false;
    }
    true
}
