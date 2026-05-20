//! Fan PWM drive and tachometer readback for the BMM Adapter.
//!
//! MMM backend: `../../../docs/MMM.md`.

use defmt::info;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{PIN_15, PIN_8, PWM_SLICE4, PWM_SLICE7};
use embassy_rp::pwm::{Config as PwmConfig, InputMode, Pwm};
use embassy_rp::Peri;
use embassy_time::Ticker;
use eth_examples::display::{FAN_PWM_PERCENT, FAN_RPM};
use eth_examples::metrics::{push, Metric, Value};
use portable_atomic::Ordering;
use ubos_monitor::FanOutput;

use crate::config::Fan;

/// PWM output driving the fan input. Construct once and pass into the
/// monitor task; the monitor sets duty via `FanOutput::set_pwm_percent`.
pub struct BoardFan {
    pwm: Pwm<'static>,
    top: u16,
}

impl BoardFan {
    #[must_use]
    pub fn new(slice: Peri<'static, PWM_SLICE4>, pin: Peri<'static, PIN_8>) -> Self {
        let mut pwm_config = PwmConfig::default();
        pwm_config.top = Fan::PWM_TOP;
        let pwm = Pwm::new_output_a(slice, pin, pwm_config);
        // Clear the display's no-fan sentinel so line 3 switches to PWM/RPM.
        FAN_PWM_PERCENT.store(0, Ordering::Relaxed);
        Self {
            pwm,
            top: Fan::PWM_TOP,
        }
    }
}

impl FanOutput for BoardFan {
    fn set_pwm_percent(&mut self, percent: u8) {
        let percent = percent.min(100);
        let compare = u32::from(self.top) * u32::from(percent) / 100;
        let mut config = PwmConfig::default();
        config.top = self.top;
        // u16 cast safe: compare <= self.top, which fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        {
            config.compare_a = compare as u16;
        }
        self.pwm.set_config(&config);
        FAN_PWM_PERCENT.store(percent, Ordering::Relaxed);
    }
}

/// Tach input peripherals consumed by [`run_tach`].
///
/// The BMM Adapter schematic names J13 pin 3 `FAN_PWM` and pin 4
/// `FAN_RPM`, but the Intel 4-wire fan spec assigns PWM to pin 4 and
/// TACH to pin 3. The firmware follows the spec, so the cable between
/// the connector and the fan must swap pins 3 and 4 relative to the
/// schematic net names.
///
/// TODO: Drop this note when a future board revision renames the nets.
pub struct TachHardware {
    pub slice: Peri<'static, PWM_SLICE7>,
    pub pin: Peri<'static, PIN_15>,
}

/// Sample tach pulses and publish RPM as a gauge. Runs forever.
pub async fn run_tach(hardware: TachHardware) -> ! {
    let tach = Pwm::new_input(
        hardware.slice,
        hardware.pin,
        Pull::Up,
        InputMode::RisingEdge,
        PwmConfig::default(),
    );

    info!(
        "fan tach: {} pulses/rev, window {} s",
        Fan::TACH_PULSES_PER_REV,
        Fan::TACH_WINDOW.as_secs(),
    );

    let mut ticker = Ticker::every(Fan::TACH_WINDOW);
    loop {
        tach.set_counter(0);
        ticker.next().await;
        let counts = u64::from(tach.counter());
        let rpm = counts * 60 / Fan::TACH_PULSES_PER_REV / Fan::TACH_WINDOW.as_secs();
        FAN_RPM.store(u32::try_from(rpm).unwrap_or(u32::MAX), Ordering::Relaxed);
        push(Metric {
            name: "board_fan_rpm",
            labels: heapless::Vec::new(),
            value: Value::Uint(rpm),
            timestamp_ms: 0,
        })
        .await;
    }
}
