//! Wire-level command type for the manager.
//!
//! `Message` is the only thing senders construct. Each variant
//! is a fact or intent — the manager interprets the message into a
//! `PauseReason` bit transition internally. Senders never spell
//! `PauseReason`; it is manager-private vocabulary.
//!
//! See `docs/ARCHITECTURE_PROPOSAL.md` §2 principle 13 ("Domain
//! messages, not bit-manipulation").

/// Wire-level command sent to the manager via `Handle::send`.
///
/// Condition events from other modules carry explicit edge direction
/// so the manager does not have to track who-asserted-what. Operator
/// intent from the API uses `Start`/`Stop`.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Message {
    /// Thermal: chip-max crossed the dangerous threshold.
    DangerousTemperature,
    /// Thermal: chip-max returned below the cooldown threshold (or the
    /// sensor recovered after a fault).
    TemperatureNormal,
    /// Cooling: fan tach signal lost or out-of-range.
    BrokenFan,
    /// Cooling: fan tach signal recovered.
    FanRestored,
    /// Stratum: every configured pool entry is unreachable.
    PoolUnreachable,
    /// Stratum: at least one pool entry reconnected.
    PoolReachable,
    /// Tuner: reported a fault it cannot recover from.
    TunerFault,
    /// Tuner: recovered.
    TunerRecovered,
    /// OTA pre-flight: asserts the OTA-update pause before erasing flash.
    OtaBegin,
    /// OTA pre-flight: clears the OTA-update pause after the update completes
    /// or is aborted.
    OtaEnd,
    /// API: operator-issued resume via `POST /api/system/resume`.
    Start,
    /// API: operator-issued pause via `POST /api/system/pause`.
    Stop,
}
