//! Internal state container and initialization gating shared by all PSU
//! drivers.

use embedded_hal_async::i2c::{ErrorKind, I2c};

use crate::Error;

#[cfg(test)]
#[path = "tests/state.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InitStatus {
    NotInit,
    Initialized,
    Failed(ErrorKind),
}

/// Per-PSU initialization routine. Implemented by each config type so the
/// shared [`ensure_initialized`] can call the right set of I2C transactions
/// without knowing the concrete IC topology.
pub(crate) trait PsuInit<I: I2c> {
    async fn run_init(i2c: &mut I, config: &Self) -> Result<(), Error<I::Error>>;
}

/// State protected by a PSU mutex. Generic over the I2C bus `I` and the
/// config type `C`, which supplies the per-PSU [`PsuInit`] logic.
pub struct State<I, C> {
    pub(crate) i2c: I,
    pub(crate) config: C,
    pub(crate) init: InitStatus,
}

impl<I, C> State<I, C> {
    pub fn new(i2c: I, config: C) -> Self {
        Self {
            i2c,
            config,
            init: InitStatus::NotInit,
        }
    }
}

pub(crate) async fn ensure_initialized<I, C, E>(state: &mut State<I, C>) -> Result<(), Error<E>>
where
    I: I2c<Error = E>,
    E: embedded_hal_async::i2c::Error,
    C: PsuInit<I>,
{
    match state.init {
        InitStatus::Initialized => return Ok(()),
        InitStatus::Failed(kind) => return Err(Error::InitFailed(kind)),
        InitStatus::NotInit => {}
    }

    if let Err(error) = C::run_init(&mut state.i2c, &state.config).await {
        let kind = match &error {
            Error::Bus(bus) => bus.kind(),
            // Init only constructs Bus or OutOfRange (the latter
            // when initial_mv is misconfigured); collapse the
            // misconfig case into Other.
            _ => ErrorKind::Other,
        };
        state.init = InitStatus::Failed(kind);
        return Err(error);
    }

    state.init = InitStatus::Initialized;
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::BmmPsuDescriptor;
    use embedded_hal_mock::eh1::i2c::Transaction;

    /// Build the I2C transaction sequence that `run_init` produces
    /// for a given `BmmPsuDescriptor`. Shared by the integration
    /// tests so the canonical init wire shape lives next to the
    /// implementation.
    const INA_CONFIG: u16 = 0b0100_0001_0010_0111;

    pub fn init_transactions(config: &BmmPsuDescriptor) -> Vec<Transaction> {
        use crate::{ina230, tps6287x};

        let dac = config
            .dcdc_voltage_range
            .voltage_to_dac(config.dcdc_initial.mv())
            .expect("BUG: initial mv must be in vrange");
        let cal_in = ina230::calibration_value(
            ina230::current_lsb_microamps(config.ina_input_max_current_a),
            config.ina_input_shunt_microohm,
        );
        let cal_out = ina230::calibration_value(
            ina230::current_lsb_microamps(config.ina_output_max_current_a),
            config.ina_output_shunt_microohm,
        );

        let mut buffer_in_config = vec![0x00u8];
        buffer_in_config.extend_from_slice(&INA_CONFIG.to_be_bytes());
        let mut buffer_in_cal = vec![0x05u8];
        buffer_in_cal.extend_from_slice(&cal_in.to_be_bytes());
        let mut buffer_out_config = vec![0x00u8];
        buffer_out_config.extend_from_slice(&INA_CONFIG.to_be_bytes());
        let mut buffer_out_cal = vec![0x05u8];
        buffer_out_cal.extend_from_slice(&cal_out.to_be_bytes());

        vec![
            Transaction::write(config.watchdog_address, vec![0x7a, 0x00]),
            Transaction::write(config.ina_input_address, buffer_in_config),
            Transaction::write(config.ina_input_address, buffer_in_cal),
            Transaction::write(config.ina_output_address, buffer_out_config),
            Transaction::write(config.ina_output_address, buffer_out_cal),
            Transaction::write(
                config.dcdc_address,
                tps6287x::encode_control1_reset().to_vec(),
            ),
            Transaction::write(
                config.dcdc_address,
                tps6287x::encode_control1(false, config.dcdc_ramp_speed).to_vec(),
            ),
            Transaction::write(
                config.dcdc_address,
                tps6287x::encode_control2(config.dcdc_voltage_range, config.dcdc_soft_start)
                    .to_vec(),
            ),
            Transaction::write(config.dcdc_address, tps6287x::encode_vset(dac).to_vec()),
            Transaction::write(config.watchdog_address, vec![0x7a, 0x01]),
            Transaction::write(config.watchdog_address, vec![0x7a, 0x00]),
        ]
    }
}
