//! Fan PWM drive and tachometer readback for the ESP32-S3.
//!
//! LEDC drives the 25 kHz PWM signal on `GPIO10` (`LowSpeed` Timer0, Channel0).
//! PCNT Unit0 counts rising edges from the tach input pin.
//!
//! `BoardFan` implements `ubos_cooling::FanOutput::set_pwm`. The
//! LEDC timer uses 10-bit duty resolution (1024 steps), giving a duty
//! granularity of ~0.1 % at 25 kHz which is well within the Intel 4-wire
//! fan specification.
//!
//! Tach readback: PCNT Unit0 is configured to increment on every rising
//! edge of the tach signal with no control signal (counter always runs).
//! The high-limit wrap is set to `i16::MAX` to defer overflow handling.
//! `BoardTach` snapshots and resets the counter on each `read_rpm` call
//! and converts pulse count to RPM over the elapsed interval.

use defmt::info;
use embassy_time::Instant;
use esp_hal::gpio::{Input, InputConfig, Pull};
use esp_hal::ledc::channel::ChannelIFace;
use esp_hal::ledc::timer::TimerIFace;
use esp_hal::ledc::{LSGlobalClkSource, Ledc, LowSpeed};
use esp_hal::pcnt::Pcnt;
use esp_hal::pcnt::channel::{CtrlMode, EdgeMode};
use esp_hal::peripherals::{GPIO10, GPIO11, LEDC, PCNT};
use esp_hal::time::Rate;
use static_cell::StaticCell;
use ubos_cooling::{FanOutput, TachReader};
use ubos_core::units::{FanPwm, FanRpm};

/// Target PWM frequency per Intel 4-wire fan specification.
const FAN_PWM_FREQ: Rate = Rate::from_khz(25);

/// Duty resolution: 10-bit gives 1024 steps at 25 kHz.
const DUTY_BITS: esp_hal::ledc::timer::config::Duty = esp_hal::ledc::timer::config::Duty::Duty10Bit;

/// Rising edges emitted by a 4-wire fan per revolution.
const TACH_PULSES_PER_REV: u64 = 2;

/// Fan PWM output backed by LEDC `LowSpeed` Timer0, Channel0 (`GPIO10`).
///
/// The LEDC peripheral and timer are held so duty updates stay valid for
/// the lifetime of the firmware.
pub struct BoardFan {
    channel: esp_hal::ledc::channel::Channel<'static, LowSpeed>,
}

impl BoardFan {
    /// Construct a `BoardFan` from the LEDC peripheral and GPIO10 for PWM.
    ///
    /// LEDC peripheral ownership is consumed here; it must be obtained from
    /// `Board::take` so it is not double-claimed.
    #[must_use]
    pub fn new(ledc: LEDC<'static>, gpio10: GPIO10<'static>) -> Self {
        static LEDC_CELL: StaticCell<Ledc<'static>> = StaticCell::new();
        static TIMER_CELL: StaticCell<esp_hal::ledc::timer::Timer<'static, LowSpeed>> =
            StaticCell::new();

        // Configure LEDC: LowSpeed Timer0 at 25 kHz, 10-bit duty.
        // `ledc` is moved into a static cell so Channel0 can hold a 'static
        // reference to the timer.
        let ledc_ref: &'static mut Ledc<'static> = LEDC_CELL.init(Ledc::new(ledc));
        ledc_ref.set_global_slow_clock(LSGlobalClkSource::APBClk);

        let mut lstimer0 = ledc_ref.timer::<LowSpeed>(esp_hal::ledc::timer::Number::Timer0);
        // Valid frequency and duty bit count; timer configuration cannot fail
        // for APB clock with this combination.
        lstimer0
            .configure(esp_hal::ledc::timer::config::Config {
                duty: DUTY_BITS,
                clock_source: esp_hal::ledc::timer::LSClockSource::APBClk,
                frequency: FAN_PWM_FREQ,
            })
            .expect("BUG: LEDC timer configuration failed");
        let lstimer0_ref: &'static _ = TIMER_CELL.init(lstimer0);

        let mut channel0 = ledc_ref.channel(esp_hal::ledc::channel::Number::Channel0, gpio10);
        // Valid timer reference and 0 % duty; cannot fail.
        channel0
            .configure(esp_hal::ledc::channel::config::Config {
                timer: lstimer0_ref,
                duty_pct: 0,
                drive_mode: esp_hal::gpio::DriveMode::PushPull,
            })
            .expect("BUG: LEDC channel configuration failed");

        Self { channel: channel0 }
    }
}

impl FanOutput for BoardFan {
    fn set_pwm(&mut self, pwm: FanPwm) {
        let percent = pwm.percent();
        // LEDC set_duty takes a percentage; it is clamp-safe here.
        // Valid timer attached; set_duty cannot return Error::Channel.
        self.channel
            .set_duty(percent)
            .expect("BUG: LEDC set_duty failed");
    }
}

/// Tachometer reader backed by PCNT Unit0 counting rising tach edges.
///
/// Holds the PCNT driver so Unit0 stays configured for the firmware
/// lifetime, and a cloned `Counter` handle for reading edge counts.
/// `read_rpm` snapshots and resets the counter, then computes RPM over
/// the elapsed time since the previous call.
pub struct BoardTach {
    /// Owned to keep PCNT Unit0 configured for the lifetime of the reader.
    pcnt: Pcnt<'static>,
    /// Edge-count handle into Unit0 used for the actual reads.
    counter: esp_hal::pcnt::unit::Counter<'static, 0>,
    /// Wall time of the previous `read_rpm` call for elapsed-time RPM math.
    last: Instant,
}

impl BoardTach {
    /// Configure PCNT Unit0 to count rising tach edges on `tach_pin`.
    ///
    /// PCNT peripheral ownership is consumed here; it must be obtained from
    /// `Board::take` so it is not double-claimed.
    #[must_use]
    pub fn new(pcnt: PCNT<'static>, tach_pin: GPIO11<'static>) -> Self {
        // Configure PCNT Unit0 to count rising edges on the tach pin.
        // No control signal: set ctrl to always-Keep so the counter always
        // increments on the positive edge.
        let pcnt_driver = Pcnt::new(pcnt);
        // High-limit wrap at i16::MAX to defer overflow; no low-limit needed.
        // High limit must be positive; i16::MAX satisfies that.
        pcnt_driver
            .unit0
            .set_high_limit(Some(i16::MAX))
            .expect("BUG: PCNT high limit invalid");
        pcnt_driver
            .unit0
            .set_filter(None)
            .expect("BUG: PCNT filter");

        // Configure the tach pin as input with pull-up (open-collector tach).
        let tach_input = Input::new(tach_pin, InputConfig::default().with_pull(Pull::Up));
        let tach_signal = tach_input.peripheral_input();

        let ch0 = &pcnt_driver.unit0.channel0;
        ch0.set_edge_signal(tach_signal);
        // No control signal; keep counter regardless of level.
        ch0.set_ctrl_mode(CtrlMode::Keep, CtrlMode::Keep);
        // Increment on rising edge, do nothing on falling edge.
        ch0.set_input_mode(EdgeMode::Hold, EdgeMode::Increment);

        pcnt_driver.unit0.clear();
        pcnt_driver.unit0.resume();

        // Counter clone for reads; the Unit itself stays here.
        let counter = pcnt_driver.unit0.counter.clone();

        info!("fan tach: {} pulses/rev", TACH_PULSES_PER_REV);
        Self {
            pcnt: pcnt_driver,
            counter,
            last: Instant::now(),
        }
    }
}

impl TachReader<1> for BoardTach {
    const LABELS: [Option<&'static str>; 1] = [None];

    fn read_rpms(&mut self) -> [FanRpm; 1] {
        let now = Instant::now();
        // A negative value means a spurious decrement; treat as zero.
        let counts = u64::try_from(self.counter.get().max(0)).unwrap_or(0);
        self.pcnt.unit0.clear();
        let elapsed_us = (now - self.last).as_micros();
        self.last = now;
        // The first call may see a near-zero interval; report 0 rather
        // than dividing by it.
        let rpm = if elapsed_us == 0 {
            0
        } else {
            counts * 60 * 1_000_000 / TACH_PULSES_PER_REV / elapsed_us
        };
        [FanRpm::from_rpm(u32::try_from(rpm).unwrap_or(u32::MAX))]
    }
}
