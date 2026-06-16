//! Fan PWM drive and tachometer readback.
//!
//! `PWM_SLICE5` channel A on GPIO10 drives the fan's PWM input at 25 kHz,
//! the rate specified by the Intel 4-wire fan spec. `BoardFan` exposes
//! the PWM output behind the `ubos_cooling::FanOutput` trait so the
//! cooling task can drive duty cycle from a PID loop.
//!
//! The three fan connectors each expose a tachometer: GPIO24
//! (`RP_TACH_A`), GPIO25 (`RP_TACH_B`), and GPIO23 (`RP_TACH_C`). None
//! of these map to a `PWM_SLICE` channel B, so embassy-rp's
//! `Pwm::new_input` cannot be used; each tach runs its own [`run_tach`]
//! task that counts rising edges via `Input::wait_for_rising_edge` in a
//! tight loop and samples the count once per `Fan::TACH_WINDOW`.

use defmt::info;
use embassy_futures::select::{select, Either};
use embassy_rp::gpio::{AnyPin, Input, Pull};
use embassy_rp::peripherals::{PIN_10, PIN_11, PWM_SLICE5};
use embassy_rp::pwm::{Config as PwmConfig, Pwm};
use embassy_rp::Peri;
use embassy_time::{Duration, Instant};
use ubos_cooling::FanOutput;
use ubos_core::fan_state;
use ubos_core::units::{FanPwm, FanRpm};
use ubos_metrics::metrics::{push, Label, LabelValue, Labels, Metric, Value};

use crate::config::Fan;

/// PWM output driving the fan input. Construct once and pass into the
/// cooling task; the cooling loop sets duty via `FanOutput::set_pwm`.
pub struct BoardFan {
    pwm: Pwm<'static>,
    top: u16,
}

impl BoardFan {
    #[must_use]
    pub fn new(
        slice: Peri<'static, PWM_SLICE5>,
        pin_a: Peri<'static, PIN_10>,
        pin_b: Peri<'static, PIN_11>,
    ) -> Self {
        let mut pwm_config = PwmConfig::default();
        pwm_config.top = Fan::PWM_TOP;
        // GPIO10 (PWM_A) and GPIO11 (PWM_B) are both channels of PWM_SLICE5;
        // driving both at one duty keeps all three fan connectors in step.
        let pwm = Pwm::new_output_ab(slice, pin_a, pin_b, pwm_config);
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
        // An inverting buffer drives the hashnode fan, so a higher duty on the
        // GPIO runs the fan slower. Invert the duty here so the commanded
        // percent matches the actual fan speed; confirmed on hardware by a
        // duty/RPM sweep.
        let duty = 100 - percent;
        let compare = u32::from(self.top) * u32::from(duty) / 100;
        let mut config = PwmConfig::default();
        config.top = self.top;
        // u16 cast safe: compare <= self.top, which fits in u16.
        #[allow(clippy::cast_possible_truncation)]
        {
            // Both channels (GPIO10/GPIO11) get the same duty.
            config.compare_a = compare as u16;
            config.compare_b = compare as u16;
        }
        self.pwm.set_config(&config);
        fan_state::store_pwm(pwm);
    }
}

/// Sample one fan's tach pulses and publish RPM as a gauge. Runs forever.
///
/// `fan` is the connector label emitted as the `fan` metric label so the
/// three connectors are distinguishable in Prometheus. The display tracks
/// a single RPM reading, so only the connector spawned with
/// `drive_display` writes [`fan_state::store_rpm`].
///
/// Runs two concurrent branches inside `select`:
/// - an edge-waiting branch that calls `wait_for_rising_edge` in a tight
///   loop and increments the window's pulse count;
/// - a deadline branch that fires every `Fan::TACH_WINDOW`, derives RPM
///   from the count, and resets for the next window.
pub async fn run_tach(pin: Peri<'static, AnyPin>, fan: &'static str, drive_display: bool) -> ! {
    let mut tach = Input::new(pin, Pull::Up);

    info!(
        "fan tach {}: {} pulses/rev, window {} s",
        fan,
        Fan::TACH_PULSES_PER_REV,
        Fan::TACH_WINDOW.as_secs(),
    );

    let mut deadline = Instant::now() + Fan::TACH_WINDOW;
    // A plain local count is correct: select resolves one branch at a time
    // within this single task, so the two branches never race.
    let mut pulse_count: u32 = 0;

    loop {
        let remaining = remaining_until(deadline);
        match select(
            tach.wait_for_rising_edge(),
            embassy_time::Timer::after(remaining),
        )
        .await
        {
            Either::First(()) => {
                pulse_count += 1;
            }
            Either::Second(()) => {
                let rpm = u64::from(pulse_count) * 60
                    / Fan::TACH_PULSES_PER_REV
                    / Fan::TACH_WINDOW.as_secs();
                if drive_display {
                    fan_state::store_rpm(FanRpm::from_rpm(u32::try_from(rpm).unwrap_or(u32::MAX)));
                }
                let mut labels = Labels::new();
                // One label, fixed capacity; the push cannot fail.
                let _ = labels.push(Label {
                    name: "fan",
                    value: LabelValue::Static(fan),
                });
                push(Metric::with_labels(
                    "board_fan_rpm",
                    labels,
                    Value::Uint(rpm),
                ))
                .await;
                pulse_count = 0;
                deadline += Fan::TACH_WINDOW;
            }
        }
    }
}

/// Compute how much time remains before `deadline`, returning zero if it has
/// already passed.
fn remaining_until(deadline: Instant) -> Duration {
    let now = Instant::now();
    if deadline > now {
        deadline - now
    } else {
        Duration::from_ticks(0)
    }
}
