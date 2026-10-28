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
//! stored one, or on a first boot [`Config::default`], with each field
//! of the board's [`Seed`] written over it whose source changed since
//! the store last took it. It then drains
//! [`COMMAND_CHANNEL`] forever. On each
//! [`Message::Update`] it persists and publishes the new value on
//! [`STATE_WATCH`].

#![cfg_attr(not(feature = "std"), no_std)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::module_name_repetitions)]

mod message;
mod versions;

#[cfg(test)]
#[path = "tests/storage.rs"]
mod tests_storage;

#[cfg(test)]
#[path = "tests/versions.rs"]
mod tests_versions;

#[cfg(test)]
#[path = "tests/validation.rs"]
mod tests_validation;

#[cfg(all(test, feature = "std"))]
#[path = "tests/factory_reset.rs"]
mod tests_factory_reset;

#[cfg(all(test, feature = "std"))]
#[path = "tests/seed.rs"]
mod tests_seed;

pub mod api;
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
use ubos_asic::HashboardDescriptor;

pub use api::ApiConfig;
pub use cooling::{
    BOUNDS as COOLING_BOUNDS, CoolingBounds, CoolingConfig, CoolingInfo, FanMode,
    INFO as COOLING_INFO, PidConfig, PidTarget, Statistic,
};
pub use fee::{FEE_DEN, FeePool, FeeSlot, MAX_FEE_PERMILLE, MIN_FEE_SLOTS, REPAY_SPACING};
pub use miner::{MinerConfig, Tuning};
pub use mining::{
    BOUNDS as MINING_BOUNDS, DefaultTuning, FrequencyVoltageTarget, MiningBounds, MiningInfo,
    PowerTarget, TunerInfo, bounds, mining_info,
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
pub use versions::StagedConfig;
pub use versions::live::Config;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::once_lock::OnceLock;
use embassy_sync::signal::Signal;
use embassy_sync::watch::{Receiver, Watch};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use portable_atomic::{AtomicBool, AtomicU64, Ordering};
use ubos_logging::{info, warn};

ubos_events::events! {
    config_changed_fixed(freq_khz: u32, voltage_mv: u32) info = "Configuration changed: {} kHz, {} mV",
    config_changed_power(power_mw: u32, mj_per_th: u32)  info = "Configuration changed: {} mW, {} mJ/TH",
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

/// Subscriber capacity for the config watch.
///
/// Covers thermal, cooling, mining/tuner, manager, API, and stratum
/// client, with two slots spare for future consumers.
const WATCH_CAPACITY: usize = 8;

/// Depth of the config command channel.
const COMMAND_CHANNEL_DEPTH: usize = 4;

/// Stack buffer size for encode and decode of stored config blobs.
///
/// Holds the widest record: the 8-byte schema-hash prefix plus the
/// postcard-encoded [`Config`] with every field at its widest. The test
/// `widest_record_fills_the_buffer` measures that record and pins this
/// value to it, so a schema change that widens the record fails the test
/// rather than a save. [`flash_storage::SCRATCH_SIZE`] is derived from
/// this value, so the flash scratch budget follows it.
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

/// Module-internal state watch carrying the committed [`Config`] —
/// the persisted truth, never the unconfirmed network preview. Single
/// writer is [`run`]. `current()` reads this, so an `/api/config`
/// update built from `current()` never carries a pending preview into
/// flash.
pub type ConfigWatch = Watch<CSMutex, Config, WATCH_CAPACITY>;

/// The committed config that the board's one config task publishes.
pub(crate) static STATE_WATCH: ConfigWatch = Watch::new();

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
pub type NetworkWatch = Watch<CSMutex, NetworkConfig, NETWORK_WATCH_CAPACITY>;

/// The effective network that the board's one config task publishes.
pub(crate) static NETWORK_WATCH: NetworkWatch = Watch::new();

/// Module-internal command channel. Single consumer is [`run`];
/// producers send via [`Handle::send`].
pub type ConfigCommandChannel = Channel<CSMutex, Message, COMMAND_CHANNEL_DEPTH>;

/// The commands that the board's one config task drains.
pub(crate) static COMMAND_CHANNEL: ConfigCommandChannel = Channel::new();

/// Receiver capacity for the update-generation watch: one per session
/// slot ([`pool::POOL_SLOTS`]), one for the pool-weights watcher, plus
/// one spare.
const GENERATION_WATCH_CAPACITY: usize = pool::POOL_SLOTS + 2;

/// Update-generation watch. The counter is bumped and published once
/// each time a successful [`Message::Update`] has been persisted and
/// published. Every stratum session task awaits it in its reconnect
/// loop so a pool change triggers an immediate teardown instead of
/// waiting for the next session error. A `Watch` rather than a
/// `Signal` because a `Signal` registers a single waker: with one
/// session task per pool slot, all of them must observe every update.
pub type GenerationWatch = Watch<CSMutex, u32, GENERATION_WATCH_CAPACITY>;

/// The update generation that the board's one config task bumps.
pub(crate) static GENERATION_WATCH: GenerationWatch = Watch::new();

/// How long [`Handle::downgrade`] waits for the config task to store the
/// record of the staged image. A config task that does not answer must
/// not keep the board from the reboot, with mining already stopped.
const DOWNGRADE_TIMEOUT: Duration = Duration::from_secs(5);

/// The answer [`run`] signals once it has downgraded the config. It
/// serves one waiter, which the OTA handler is: the handler calls
/// [`Handle::downgrade`] under the OTA target lock, which admits one
/// upload at a time.
static DOWNGRADE_DONE: Signal<CSMutex, ()> = Signal::new();

/// The cell that holds the fee slot a board hands to [`run`]. Named so
/// a host test can declare one of its own for a [`Handle`].
pub type FeeSlotCell = OnceLock<FeeSlot>;

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
    Handle::new(
        &STATE_WATCH,
        &NETWORK_WATCH,
        &COMMAND_CHANNEL,
        &GENERATION_WATCH,
        &fee::FEE_SLOT,
    )
}

/// Consumer-side handle for the config subsystem.
///
/// `Copy` because it holds only `'static` references. Every consumer
/// task takes its own value.
///
/// Every method reads one of these fields, so a handle answers for the
/// channels it was built over and for nothing else.
#[derive(Clone, Copy)]
pub struct Handle {
    state: &'static ConfigWatch,
    network: &'static NetworkWatch,
    commands: &'static ConfigCommandChannel,
    generation: &'static GenerationWatch,
    fee: &'static FeeSlotCell,
}

impl Handle {
    /// A handle over these channels. Firmware calls [`handle`] instead,
    /// which answers for the set the board runs on. A host test that
    /// drives a config consumer builds a handle over channels it
    /// declared itself, and then no other test can observe what it
    /// publishes.
    #[must_use]
    pub const fn new(
        state: &'static ConfigWatch,
        network: &'static NetworkWatch,
        commands: &'static ConfigCommandChannel,
        generation: &'static GenerationWatch,
        fee: &'static FeeSlotCell,
    ) -> Self {
        Self {
            state,
            network,
            commands,
            generation,
            fee,
        }
    }

    /// Returns the most recently published config, falling back to
    /// [`Config::default`] if nothing has been published yet.
    #[must_use]
    pub fn current(&self) -> Config {
        // Fall back to Default before the first publish; the window is
        // tiny but must not return garbage.
        self.state.try_get().unwrap_or_default()
    }

    /// The entry a session slot runs: the operator's entry below the
    /// fee slot, the fee pool's entry at it, and `None` for a slot the
    /// list does not fill and past the board's slots. Owned, because
    /// the fee entry is built on each read over the current config.
    #[must_use]
    pub fn pool_slot(&self, slot: usize) -> Option<PoolEntry> {
        if let Some(fee) = self.fee.try_get()
            && fee.slot == slot
        {
            return Some(fee.pool.entry());
        }
        self.current().pool.get(slot).cloned()
    }

    /// The config as `GET /api/config` serves it: the current config
    /// with the fee entry and its share beside the operator's list.
    #[must_use]
    pub fn view(&self) -> ConfigUpdate {
        let mut view = ConfigUpdate::from_config(&self.current());
        if let Some(fee) = self.fee.try_get() {
            view.fee = Some(fee.pool.entry());
            view.fee_permille = Some(fee.pool.permille);
        }
        view
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
        self.network
            .receiver()
            .expect("BUG: NETWORK_WATCH receiver slots exhausted; raise NETWORK_WATCH_CAPACITY")
    }

    /// Submit a config update. The task running in [`run`] persists the
    /// new value then publishes it on `STATE_WATCH`.
    pub async fn send(&self, message: Message) {
        self.commands.send(message).await;
    }

    /// Submit a command without awaiting queue space. Returns `false`
    /// when the channel is full so a non-async caller, such as an HTTP
    /// handler, can surface a busy response instead of blocking.
    #[must_use]
    pub fn try_send(&self, message: Message) -> bool {
        self.commands.try_send(message).is_ok()
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
        self.generation.receiver().expect(
            "BUG: GENERATION_WATCH receiver slots exhausted; raise GENERATION_WATCH_CAPACITY",
        )
    }
}

/// Partial-update cooling section of [`ConfigUpdate`].
///
/// Only the operator-tunable cooling fields are exposed; the rest of
/// [`CoolingConfig::pid`] (the PID gains) is factory-set and never patched
/// from the UI. Each field is `Option` so one can be set without the others.
/// `mode` and `fixed_fan_pwm` are separate fields rather than one payload
/// variant so the UI can edit the duty without leaving auto control.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct CoolingUpdate {
    /// Replacement for [`CoolingConfig::mode`]; `None` leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<FanMode>,
    /// Replacement for [`CoolingConfig::target`]; `None` leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PidTarget>,
    /// Replacement for [`CoolingConfig::fixed_fan_pwm`] (the fixed-mode duty,
    /// 0-100); `None` leaves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_fan_pwm: Option<u8>,
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
    /// Replacement value for [`Config::thermal`]; `None` means leave unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thermal: Option<ThermalConfig>,
    /// Partial cooling update (mode, setpoint, fixed duty, and fan floor);
    /// `None` means leave unchanged. The full [`CoolingConfig`] is not
    /// exposed because the PID gains are factory-tuned and must not be
    /// overwritten from the UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooling: Option<CoolingUpdate>,
    /// Replacement value for [`Config::miner`]; `None` means leave unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub miner: Option<MinerConfig>,
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

    /// Produce a fully-populated `ConfigUpdate` from a live [`Config`].
    ///
    /// The base of the `GET /api/config` body. The `api.password` field is
    /// set to an empty string so the stored credential is never transmitted
    /// to the client. `fee` and `fee_permille` are `None`, because a
    /// `Config` holds no fee slot; [`Handle::view`] fills them from the
    /// slot the board installed.
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

    /// Validate all present fields against the bounds and against `current`.
    ///
    /// `current` is the live [`Config`] snapshot the update will be merged
    /// into; the caller is responsible for reading it once and passing it
    /// in so the function is pure and the read order is explicit.
    ///
    /// An incoming `thermal` must name the same reading kinds as `current`,
    /// because the kinds belong to the machine.
    ///
    /// For `cooling.target`, the dangerous temperature is the threshold of
    /// its kind in the incoming `thermal` field when present, otherwise in
    /// `current.thermal`.
    ///
    /// The cross-field rule covers both directions:
    /// - When `cooling.target` is in the update, the incoming target must
    ///   name a kind the machine reads and stay strictly below its threshold.
    /// - When only `thermal` is in the update, the current `cooling.target`
    ///   must still be strictly below the incoming threshold of its kind.
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

impl Default for Config {
    /// Factory defaults for the BMM101 board.
    ///
    /// Provided so the test suite (and any caller without a specific
    /// board) has a stable baseline; the firmware uses
    /// [`Config::for_board`] with its own descriptor instead.
    fn default() -> Self {
        Self::for_board(&ubos_asic::BMM101_HASHBOARD, DefaultTuning::Fixed)
    }
}

impl Config {
    /// Factory defaults for `board`, with `default_tuning` as the tuning.
    ///
    /// Not a `const fn` because [`ApiConfig`]'s default cannot be `const`
    /// (heapless 0.9 `String::push_str` is not `const fn`). The value is
    /// computed once at boot; it is cheap.
    #[must_use]
    pub fn for_board(board: &'static HashboardDescriptor, default_tuning: DefaultTuning) -> Self {
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
                // The halt thresholds live on the board descriptor rather
                // than a single hardcoded default, so a board comes up with
                // a threshold for each kind of reading it measures: its chip
                // family's junction ceiling, the temperature its hashboard
                // may reach, or both.
                dangerous: TemperatureDescriptor::dangerous_of(board),
                sample_interval_ms: thermal::DEFAULT_SAMPLE_INTERVAL_MS,
                cooldown_temp: thermal::DEFAULT_COOLDOWN_TEMP,
            },
            cooling: CoolingConfig {
                // Every board boots under PID control; a fixed duty is an
                // explicit operator choice, never a factory default.
                mode: FanMode::Auto,
                // The board's setpoints live on its descriptor, see
                // `PidTarget::default_for`.
                // Runtime-editable via the UI afterwards.
                target: PidTarget::default_for(board),
                fixed_fan_pwm: cooling::DEFAULT_FIXED_FAN_PWM,
                pid: PidConfig {
                    min_fan_pwm: board.cooling.min_fan_pwm,
                    input_smoother_half_decay_secs: board.cooling.input_smoother_half_decay_secs,
                    ..PidConfig::default()
                },
            },
            miner: MinerConfig {
                tuning: default_tuning.tuning(board),
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
    /// change shifts this hash. The boot compares the 8-byte prefix of
    /// the stored record against this constant, and against the hashes
    /// of the older versions it can upgrade from, and falls back to
    /// defaults on a record in no schema it reads.
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
    /// via [`Config::default`]; they are not re-validated here.
    ///
    /// # Errors
    ///
    /// Returns the first failing field's static error string.
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

/// Reconfigure task for the config subsystem.
///
/// On boot, loads the stored [`Config`] via `storage` and validates it
/// against `info.bounds`. A record that an older version stored is
/// upgraded to the live schema and stored again in it. On
/// [`LoadError::SchemaMismatch`] (a record in no schema this build
/// reads), [`LoadError::Corrupt`]
/// (partial write before power loss), an I/O error, or a failed
/// validation, falls back to [`Config::for_board`] with
/// `info.default_tuning` and logs the reason. An upgrade and each
/// fallback record an event, so the operator reads what an update did
/// to the config. `Ok(None)` means the key was never written and is
/// treated as a clean first-boot: defaults, with no warning.
///
/// A board can hand in a `seed` of settings it recovered from the
/// firmware it replaced. Each seed field is written over the config
/// when its fingerprint differs from the one the store last took, and
/// every field is new to a store that has taken none. The result is
/// persisted at once with the new fingerprints. So a setting changed
/// under the other firmware reaches the config once, and a setting the
/// operator has since changed here stays as they left it. A factory
/// reset keeps the fingerprints, so the seed stays out until its
/// source changes. A fallback for any reason but an I/O error takes
/// every seed field, whatever the fingerprints say. A seed whose result
/// fails validation is dropped with a warning.
///
/// A board hands in the `fee` slot it runs, from [`FeeSlot::last_of`],
/// and the handle answers for it from then on: [`Handle::pool_slot`]
/// builds its entry, [`Handle::view`] serves it beside the operator's
/// list. `None` on a board with no fee slot and on an off build.
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
///
/// A [`Message::Downgrade`] stores the record that the image an OTA
/// staged reads, answers [`Handle::downgrade`], and drops every command
/// after it until the handler reboots the board into that image.
pub async fn run<S: Storage>(
    mut storage: S,
    board: &'static HashboardDescriptor,
    info: &'static MiningInfo,
    seed: Option<Seed>,
    fee: Option<FeeSlot>,
) -> ! {
    // Before the first publish, which every reader of the fee slot
    // waits for, so none of them can read an empty slot and park.
    if let Some(fee) = fee {
        fee::install(fee);
    }
    let mut committed = load_initial(&mut storage, board, info, seed).await;
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
                committed = factory_reset(&mut storage, board, info.default_tuning).await;
                NETWORK_WATCH.sender().send(committed.network.clone());
                ubos_core::reboot::request_reboot();
            }
            Message::Downgrade(staged) => {
                store_downgraded(&committed, staged, &mut storage).await;
                DOWNGRADE_DONE.signal(());
                drop_until_reboot().await;
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
async fn drop_until_reboot() -> ! {
    loop {
        let _ = COMMAND_CHANNEL.receive().await;
        warn!("config: an update is installing, the change is not saved");
    }
}

/// Task shell around [`run`], which drives the persisted config store.
/// Takes the board's storage backend, which bottoms out in its own flash
/// driver, the hashboard descriptor, and the firmware's [`MiningInfo`],
/// whose bounds the store validates against and whose default tuning a
/// new config starts on. The task's `seed` argument holds the settings
/// the board recovered from the firmware it replaced, when it has any to
/// offer; a board with none spawns it with `None`.
#[macro_export]
macro_rules! config_task {
    ($storage:ty, $board:expr, $info:expr, $fee:expr) => {
        #[embassy_executor::task]
        async fn config_task(storage: $storage, seed: Option<ubos_config::Seed>) -> ! {
            ubos_config::run(storage, $board, $info, seed, $fee).await
        }
    };
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

/// Load the persisted [`Config`], or on a first boot the defaults, with
/// the new fields of the `seed` written over it, or fall back to
/// [`Config::for_board`].
///
/// The buffer is sized to match [`encode_blob`]'s budget so any blob
/// that could have been stored fits without reallocation. All failure
/// paths converge on defaults so the firmware always boots with a
/// valid config. A seed field is new when its fingerprint differs from
/// the taken one, so a first boot with no taken record takes them all.
///
/// A stored config that this build cannot read takes every seed field
/// over the defaults, whatever the store took before, and is stored at
/// once. The operator's config is gone then, and the settings the board
/// recovered are the best values it knows. A load that fails with an
/// I/O error takes no seed, so a flash fault never writes.
async fn load_initial<S: Storage>(
    storage: &mut S,
    board: &'static HashboardDescriptor,
    info: &'static MiningInfo,
    seed: Option<Seed>,
) -> Config {
    // The buffer is scoped to the load, so the task does not hold it
    // across the seed writes. An empty store reads as the defaults, so
    // `None` is a record this build cannot read.
    let loaded = {
        let mut buffer = [0u8; LOAD_BUFFER_SIZE];
        match storage.load(CONFIG_KEY, &mut buffer).await {
            Ok(Some(blob)) => decode_stored::<S::Error>(blob, board, info),
            Ok(None) => {
                info!("config: no stored config, using defaults");
                Some(Loaded::Live(Config::for_board(board, info.default_tuning)))
            }
            Err(_) => {
                warn!("config: load failed, using defaults");
                crate::events::config_unusable();
                return Config::for_board(board, info.default_tuning);
            }
        }
    };
    // An upgraded record is stored again in this build's schema, so a later
    // build reads it after it drops the version that wrote it.
    let stored = match loaded {
        Some(Loaded::Upgraded(config)) => {
            persist(&config, storage).await;
            Some(config)
        }
        Some(Loaded::Live(config)) => Some(config),
        None => None,
    };
    let Some(seed) = &seed else {
        return stored.unwrap_or_else(|| Config::for_board(board, info.default_tuning));
    };
    // The boot config is scoped to the merge, so the task holds one config
    // across the writes below rather than two.
    let (config, taken, store) = {
        let lost = stored.is_none();
        let base = stored.unwrap_or_else(|| Config::for_board(board, info.default_tuning));
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

/// The config in a stored `blob`, or `None` after a logged reason when
/// it does not decode or fails validation.
fn decode_stored<E>(
    blob: &[u8],
    board: &'static HashboardDescriptor,
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
        config.miner.tuning = info.default_tuning.tuning(board);
    }
    // A firmware update can change the kinds of reading a machine
    // measures, and a threshold is kept only for a kind the machine reads.
    // Only the thresholds are replaced then, as the tuning is.
    let dangerous = TemperatureDescriptor::dangerous_of(board);
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
        config.cooling.target = PidTarget::default_for(board);
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

/// Log the variant-specific reason a stored blob could not be decoded,
/// and record its event.
///
/// Each arm produces a different diagnostic so operators can tell a
/// firmware-upgrade schema bump apart from a flash corruption event.
/// The event names only the schema bump, because an update caused it,
/// and a damaged or an unreadable record is the same loss to the
/// operator.
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
    match &new.miner.tuning {
        Tuning::Fixed(target) => {
            crate::events::config_changed_fixed(target.frequency_khz, target.voltage_mv);
        }
        Tuning::Power(target) => {
            crate::events::config_changed_power(
                target.input_power_mw,
                target.efficiency_mj_per_th.get(),
            );
        }
    }
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

/// Reset the stored config to [`Config::for_board`] with
/// `default_tuning` and publish it.
///
/// Persists the defaults, refreshes [`STATE_WATCH`], and returns the
/// new value. Returning the config keeps the persisted, published, and
/// in-memory values provably identical and lets tests drive the real
/// reset path.
async fn factory_reset<S: Storage>(
    storage: &mut S,
    board: &'static HashboardDescriptor,
    default_tuning: DefaultTuning,
) -> Config {
    let config = Config::for_board(board, default_tuning);
    persist(&config, storage).await;
    STATE_WATCH.sender().send(config.clone());
    info!("config: factory reset to defaults");
    crate::events::factory_reset();
    config
}

/// Persist `config` to `storage`, and say whether it reached the store.
/// A failure is logged and otherwise swallowed: an in-memory-only update
/// is preferable to leaving mining paused.
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
