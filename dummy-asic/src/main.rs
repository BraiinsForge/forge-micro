//! Dummy ASIC peer for the Seeed XIAO ESP32-S3.
//!
//! Reads bytes at 115200 baud until a newline, then echoes the complete
//! line back. Used as a hardware-in-the-loop test target for verifying
//! UART connectivity on the eth-examples boards. Will be repurposed as
//! a stratum mining target in a follow-up commit.
//!
//! Pin mapping for XIAO ESP32-S3 silk labels:
//!   D9  = GPIO8 = TX
//!   D10 = GPIO9 = RX

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::{Duration, Ticker};
use embedded_io_async::{Read as AsyncRead, Write as AsyncWrite};
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart};
use {defmt_rtt as _, esp_backtrace as _};

defmt::timestamp!("{=u64}", embassy_time::Instant::now().as_secs());

mod config;

esp_bootloader_esp_idf::esp_app_desc!();

/// Maximum line length before discarding and resetting the buffer.
const MAX_LINE_LEN: usize = 256;

/// Period between die temperature log messages.
const TEMP_LOG_PERIOD: Duration = Duration::from_secs(30);

// The eth-examples firmware crates feed readings into Prometheus via
// `eth_examples::temperature::run_temperature`. dummy-asic has no
// metrics channel, so it logs directly on a longer cadence.
#[embassy_executor::task]
async fn temperature_log_task(mut tsens: esp32s3_tsens::Tsens<'static>) -> ! {
    let mut ticker = Ticker::every(TEMP_LOG_PERIOD);
    loop {
        if let Some(millicelsius) = tsens.read_millicelsius().await {
            defmt::info!("dummy-asic: die temp {} m°C", millicelsius);
        }
        ticker.next().await;
    }
}

#[embassy_executor::task]
async fn echo_task(mut uart: Uart<'static, esp_hal::Async>) -> ! {
    defmt::info!(
        "dummy-asic: ready on UART1 at {} baud",
        eth_examples::dummy_asic::BAUD_RATE
    );

    let mut buffer = [0u8; MAX_LINE_LEN];
    let mut position = 0;

    loop {
        let mut byte = [0u8; 1];
        match AsyncRead::read(&mut uart, &mut byte).await {
            Ok(0) => {}
            Ok(_) => {
                let received = byte[0];
                buffer[position] = received;
                position += 1;

                if received == b'\n' {
                    // Echo the complete line including the newline.
                    if let Err(error) = AsyncWrite::write_all(&mut uart, &buffer[..position]).await
                    {
                        defmt::error!("dummy-asic: write error: {}", defmt::Debug2Format(&error));
                    }
                    position = 0;
                } else if position >= MAX_LINE_LEN {
                    // Buffer overflow without newline: discard and reset.
                    defmt::warn!("dummy-asic: line too long, discarding {} bytes", position);
                    position = 0;
                }
            }
            Err(error) => {
                defmt::error!(
                    "dummy-asic: read error: {}, resetting buffer",
                    defmt::Debug2Format(&error)
                );
                position = 0;
            }
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let hw = config::Board::take(peripherals);

    let timg0 = TimerGroup::new(hw.timg0);
    esp_rtos::start(timg0.timer0);

    let uart_config = UartConfig::default().with_baudrate(eth_examples::dummy_asic::BAUD_RATE);
    // Valid UART peripheral and default-compatible baud rate; cannot fail.
    let uart = Uart::new(hw.uart1, uart_config)
        .expect("BUG: UART configuration failed")
        .with_tx(hw.uart_tx)
        .with_rx(hw.uart_rx)
        .into_async();

    let tsens = esp32s3_tsens::Tsens::new(hw.sens, esp32s3_tsens::Range::Standard).await;
    spawner.must_spawn(temperature_log_task(tsens));
    spawner.must_spawn(echo_task(uart));
}
