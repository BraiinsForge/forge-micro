//! Fan PWM drive and tachometer readback.
//!
//! Slice 4 channel A on `PIN_8` drives the fan's PWM input at 25 kHz,
//! the rate specified by the Intel 4-wire fan spec. `BoardFan` exposes
//! the PWM output behind the `ubos_monitor::FanOutput` trait so the
//! monitor task can drive duty cycle from a PID loop.
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
use eth_examples::metrics::{push, Metric, Value};
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
        push(Metric {
            name: "board_fan_rpm",
            labels: heapless::Vec::new(),
            value: Value::Uint(rpm),
            timestamp_ms: 0,
        })
        .await;
    }
}
