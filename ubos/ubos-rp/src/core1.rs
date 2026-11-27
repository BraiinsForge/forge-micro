// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! The core-1 miner-serial spawn.

/// Pin the ubos-mining serial worker to core 1 so embassy-net on core 0 stays
/// responsive. The macro builds the `BufferedUart` and wraps it in
/// [`MinerUart`](crate::uart::MinerUart), then hands the value to the board's
/// executor body. `BufferedUart` must be constructed on the core that polls
/// it, which is why the whole thing runs inside `spawn_core1`. Both RP cores
/// have their own NVIC, so an `init_buffers` unmask on one core and the write
/// path's pend on the other land on different controllers and the HW FIFO is
/// never written.
///
/// The `run` arm chooses which executor drives core 1:
/// `run_cortex_m_executor::<1, _>` accumulates idle ticks for the busy-ratio
/// metric, and a board with a hand-managed executor passes its own.
///
/// ```ignore
/// ubos_rp::spawn_miner_core1!(
///     core1 = hw.core1,
///     stack = CORE1_STACK,
///     irqs = Irqs,
///     uart = (hw.uart, hw.uart_tx, hw.uart_rx),
///     run = |miner_serial| {
///         ubos_rp::executor::run_cortex_m_executor::<1, _>(|spawner: Spawner| {
///             spawner.spawn(unwrap!(mining::serial_task(
///                 &mining::CHAINS[0],
///                 &PARK,
///                 miner_serial,
///                 &ASIC_SESSION,
///                 driver,
///             )));
///         });
///     },
/// );
/// ```
///
/// The macro expands into the caller's crate, so `static_cell::StaticCell`
/// resolves against the caller's own dependencies. Every crate that invokes
/// `spawn_miner_core1!` must depend on `static_cell`.
#[macro_export]
macro_rules! spawn_miner_core1 {
    (
        core1 = $core1:expr,
        stack = $stack:expr,
        irqs = $irqs:expr,
        uart = ($uart:expr, $tx:expr, $rx:expr),
        run = |$serial:ident| $body:block $(,)?
    ) => {{
        embassy_rp::multicore::spawn_core1($core1, $stack.take(), move || {
            let $serial = {
                static TX_BUF: static_cell::StaticCell<[u8; $crate::UART_RING_BUFFER_SIZE]> =
                    static_cell::StaticCell::new();
                static RX_BUF: static_cell::StaticCell<[u8; $crate::UART_RING_BUFFER_SIZE]> =
                    static_cell::StaticCell::new();
                let mut config = embassy_rp::uart::Config::default();
                config.baudrate = ubos_mining::consts::INITIAL_BAUDRATE;
                let uart = embassy_rp::uart::BufferedUart::new(
                    $uart,
                    $tx,
                    $rx,
                    $irqs,
                    TX_BUF.init([0; $crate::UART_RING_BUFFER_SIZE]),
                    RX_BUF.init([0; $crate::UART_RING_BUFFER_SIZE]),
                    config,
                );
                $crate::uart::MinerUart::new(uart)
            };
            $body
        });
    }};
}
