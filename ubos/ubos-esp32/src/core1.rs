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
/// responsive. The macro builds the `Uart` and wraps it in
/// [`MinerUart`](crate::uart::MinerUart), then hands the value to the board's
/// executor body. `Uart` must be constructed on the core that polls it,
/// which is why the whole thing runs inside `esp_rtos::start_second_core`.
///
/// The `run` arm carries the executor and idle-accounting choice: it builds
/// the second core's executor, spawns the miner-serial task onto it, and
/// runs it through `run_with_callbacks` with
/// [`IdleAccounting`](crate::IdleAccounting)`::<1>` so core 1's idle ticks
/// are recorded for the cpu-load metric.
///
/// ```ignore
/// ubos_esp32::spawn_miner_core1!(
///     cpu_ctrl = hw.cpu_ctrl,
///     sw_interrupt = software_interrupts.software_interrupt1,
///     stack = APP_CORE_STACK,
///     uart = (hw.uart0, hw.uart_tx, hw.uart_rx),
///     run = |miner_serial| {
///         let executor = APP_EXECUTOR.init(esp_rtos::embassy::Executor::new());
///         executor.run_with_callbacks(
///             |app_spawner| {
///                 app_spawner.spawn(unwrap!(mining::serial_task(miner_serial, &ASIC_SESSION)));
///             },
///             ubos_esp32::IdleAccounting::<1>::new(),
///         );
///     },
/// );
/// ```
#[macro_export]
macro_rules! spawn_miner_core1 {
    (
        cpu_ctrl = $cpu_ctrl:expr,
        sw_interrupt = $sw_int:expr,
        stack = $stack:expr,
        uart = ($uart:expr, $tx:expr, $rx:expr),
        run = |$serial:ident| $body:block $(,)?
    ) => {{
        esp_rtos::start_second_core(
            $cpu_ctrl,
            $sw_int,
            $stack.init(esp_hal::system::Stack::new()),
            // The uart, tx, and rx expressions move into the closure
            // individually, so passing fields of one struct captures only
            // those fields and leaves the rest of the struct with the caller.
            move || {
                let $serial = {
                    let uart_config = esp_hal::uart::Config::default()
                        // Valid baud rate for UART0; cannot fail.
                        .with_baudrate(ubos_mining::consts::INITIAL_BAUDRATE);
                    // Valid UART0 peripheral with default-compatible baud rate; cannot fail.
                    let uart = esp_hal::uart::Uart::new($uart, uart_config)
                        .expect("BUG: UART0 configuration failed")
                        .with_tx($tx)
                        .with_rx($rx)
                        .into_async();
                    $crate::uart::MinerUart::new(uart)
                };
                $body
            },
        );
    }};
}
