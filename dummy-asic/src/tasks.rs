//! `PRO_CPU` workload: UART reader and writer.
//!
//! The UART is split so reader and writer own `UartRx` / `UartTx` halves
//! respectively and never contend.

use embassy_futures::select::{select, Either};
use esp_hal::uart::{UartRx, UartTx};
use portable_atomic::Ordering;

use crate::protocol::{
    read_request_frame, write_share_frame, write_temp_frame, Request, BAUD_RATE,
};
use crate::{CURRENT_WORK, LATEST_TEMP_MILLI_CELSIUS, SOLUTIONS, TEMP_READ_PENDING};

/// Reader task on core 0.  Parses request frames and dispatches them to the
/// shared state the writer and solver consume.
#[embassy_executor::task]
pub(crate) async fn reader_task(mut rx: UartRx<'static, esp_hal::Async>) -> ! {
    defmt::info!("dummy-asic: ready on UART1 at {} baud", BAUD_RATE);

    loop {
        match read_request_frame(&mut rx).await {
            Request::Work(work) => {
                defmt::debug!(
                    "dummy-asic: received work, target={} zero bytes",
                    work.target_leading_zero_bytes
                );
                CURRENT_WORK.sender().send(work);
            }
            Request::ReadTemp => {
                // `send` awaits when the 1-slot channel is full.  Bounds
                // outstanding temp reads at one in flight, which keeps the
                // host's request-response contract simple.
                TEMP_READ_PENDING.send(()).await;
            }
        }
    }
}

/// Writer task on core 0.  Races pending temperature reads against completed
/// solutions and emits the corresponding response frame.  The sole owner of
/// `UartTx`, so writes never contend.
#[embassy_executor::task]
pub(crate) async fn writer_task(mut tx: UartTx<'static, esp_hal::Async>) -> ! {
    loop {
        match select(TEMP_READ_PENDING.receive(), SOLUTIONS.receive()).await {
            Either::First(()) => {
                let milli_celsius = LATEST_TEMP_MILLI_CELSIUS.load(Ordering::Relaxed);
                defmt::debug!("dummy-asic: emit temp {} m°C", milli_celsius);
                write_temp_frame(&mut tx, milli_celsius).await;
            }
            Either::Second(solution) => {
                defmt::debug!("dummy-asic: found nonce={:08x}", solution.nonce);
                write_share_frame(&mut tx, &solution).await;
            }
        }
    }
}
