//! Fan PWM drive and tachometer readback.
//!
//! The BMM Adapter has `FAN_PWM` on J13 pin 3 and `FAN_RPM` on
//! J13 pin 4, but the Intel 4-wire fan spec has TACH
//! on pin 3 and PWM on pin 4. Here we work with it
//! according to the spec, so the cable between the connector and
//! the fan must swap pins 3 and 4 relative to the schematic net
//! names.
//!
//! Slice 4 channel A on `PIN_8` drives the fan's PWM input at 25 kHz,
//! the rate specified by the Intel 4-wire fan spec. `BoardFan` exposes
//! the PWM output behind the `ubos_cooling::FanOutput` trait so the
//! cooling task can drive duty cycle from a PID loop.
//!
//! Slice 7 channel B on `PIN_15` runs in `RisingEdge` divmode and
//! counts tach pulses. Standard 4-wire fans emit two pulses per
//! revolution, so RPM is `pulses × 60 / 2` over the sample window.
//! The tach output is open-collector, so the pad's internal pull-up
//! is enabled to idle the line high between pulses.

use defmt::info;
use embassy_rp::gpio::Pull;
use embassy_rp::peripherals::{PIN_15, PIN_8, PWM_SLICE4, PWM_SLICE7};
use embassy_rp::pwm::{Config as PwmConfig, InputMode, Pwm};
use embassy_rp::Peri;
use embassy_time::Ticker;
use ubos_cooling::FanOutput;
use ubos_core::fan_state;
use ubos_core::units::{FanPwm, FanRpm};
use ubos_metrics::metrics::{push, Metric, Value};

use crate::config::Fan;

/// PWM output driving the fan input. Construct once and pass into the
/// cooling task; the cooling loop sets duty via `FanOutput::set_pwm`.
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
        fan_state::store_pwm(FanPwm::ZERO);
        Self {
            pwm,
            top: Fan::PWM_TOP,
        }
    }
}

impl FanOutput for BoardFan {
    fn set_pwm(&mut self, pwm: FanPwm) {
        let percent = pwm.percent();
        let compare = u32::from(self.top) * u32::from(percent) / 100;
        let mut config = PwmConfig::default();
        config.top = self.top;
        // u16 cast safe: compare <= self.top, which fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        {
            config.compare_a = compare as u16;
        }
        self.pwm.set_config(&config);
        fan_state::store_pwm(pwm);
    }
}

/// Tach input peripherals consumed by [`run_tach`].
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
        fan_state::store_rpm(FanRpm::from_rpm(u32::try_from(rpm).unwrap_or(u32::MAX)));
        push(Metric {
            name: "board_fan_rpm",
            labels: heapless::Vec::new(),
            value: Value::Uint(rpm),
            timestamp_ms: 0,
        })
        .await;
    }
}
