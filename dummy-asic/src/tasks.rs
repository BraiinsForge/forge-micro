//! `PRO_CPU` workload: UART reader and writer tasks.
//!
//! The UART is split so reader and writer own `UartRx` / `UartTx` halves
//! respectively and never contend.
//!
//! Dummy ASIC: `../../docs/DUMMY_ASIC.md`.

use esp_hal::uart::{UartRx, UartTx};

use crate::protocol::{read_work_frame, write_share_frame, BAUD_RATE};
use crate::{CURRENT_WORK, SOLUTIONS};

/// Reader task on core 0.  Parses Work frames and publishes them to the
/// solver.
#[embassy_executor::task]
pub(crate) async fn reader_task(mut rx: UartRx<'static, esp_hal::Async>) -> ! {
    defmt::info!("dummy-asic: ready on UART1 at {} baud", BAUD_RATE);

    loop {
        let work = read_work_frame(&mut rx).await;
        defmt::debug!("dummy-asic: received work");
        CURRENT_WORK.sender().send(work);
    }
}

/// Writer task on core 0.  Emits a Share frame for every completed solution.
/// Sole owner of `UartTx`, so writes never contend.
#[embassy_executor::task]
pub(crate) async fn writer_task(mut tx: UartTx<'static, esp_hal::Async>) -> ! {
    loop {
        let solution = SOLUTIONS.receive().await;
        defmt::debug!("dummy-asic: found nonce={:08x}", solution.nonce);
        write_share_frame(&mut tx, &solution).await;
    }
}
