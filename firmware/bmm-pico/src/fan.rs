//! Fan PWM drive and tachometer readback.
//!
//! Slice 4 channel A on `PIN_8` drives the fan's PWM input at 25 kHz,
//! the rate specified by the Intel 4-wire fan spec. Duty cycle is
//! fixed here; the temperature-driven curve is intentionally absent
//! so the task stays self-contained.
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

use crate::config::Fan;

/// PWM and tach peripherals consumed by [`run`].
pub struct FanHardware {
    pub pwm_slice: Peri<'static, PWM_SLICE4>,
    pub pwm_pin: Peri<'static, PIN_8>,
    pub tach_slice: Peri<'static, PWM_SLICE7>,
    pub tach_pin: Peri<'static, PIN_15>,
}

/// Drive the fan at a fixed duty cycle and publish tach-derived RPM
/// as a gauge. Runs forever.
pub async fn run(hardware: FanHardware) -> ! {
    let mut pwm_config = PwmConfig::default();
    pwm_config.top = Fan::PWM_TOP;
    pwm_config.compare_a = Fan::PWM_COMPARE;
    // `_pwm` is held for the lifetime of this task. Dropping it would
    // disable the slice and re-park the pin as a pull-down GPIO.
    let _pwm = Pwm::new_output_a(hardware.pwm_slice, hardware.pwm_pin, pwm_config);

    let tach = Pwm::new_input(
        hardware.tach_slice,
        hardware.tach_pin,
        Pull::Up,
        InputMode::RisingEdge,
        PwmConfig::default(),
    );

    info!(
        "fan: 25 kHz PWM, duty {}/{} ({}%)",
        Fan::PWM_COMPARE,
        Fan::PWM_TOP + 1,
        Fan::DUTY_PERCENT,
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
