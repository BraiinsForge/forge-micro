//! XIAO ESP32-S3 dummy ASIC: receives mining Work over UART, searches nonces
//! with the hardware SHA accelerator, returns Share packets.
//!
//! Protocol, runtime model, and flashing: `../../docs/DUMMY_ASIC.md`.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::watch::Watch;
use esp_hal::clock::CpuClock;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::system::Stack;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::{Config as UartConfig, Uart};
use static_cell::StaticCell;
use {defmt_rtt as _, esp_backtrace as _};

defmt::timestamp!("{=u64}", embassy_time::Instant::now().as_secs());

mod config;
mod protocol;
mod solver;
mod tasks;

use protocol::{Solution, Work, BAUD_RATE};
use solver::solver_task;
use tasks::{reader_task, writer_task};

esp_bootloader_esp_idf::esp_app_desc!();

// ── Shared state ──────────────────────────────────────────────────────────────

/// Current work item, broadcast to the solver on every new Work frame.
/// Single-receiver Watch published by the reader task.
pub(crate) static CURRENT_WORK: Watch<CriticalSectionRawMutex, Work, 1> = Watch::new();

/// Solutions produced by the solver, consumed by the writer task.
pub(crate) static SOLUTIONS: Channel<CriticalSectionRawMutex, Solution, 4> = Channel::new();

/// Static storage for the second-core embassy executor.
static APP_EXECUTOR: StaticCell<esp_rtos::embassy::Executor> = StaticCell::new();

/// Stack allocation for the `APP_CPU` scheduler thread.
static APP_CORE_STACK: StaticCell<Stack<8192>> = StaticCell::new();

// ── Entry point ───────────────────────────────────────────────────────────────

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    // Crank the CPU to its maximum so the SHA peripheral's APB-side
    // dispatch and the Xtensa busy-poll between hardware blocks both
    // run as fast as the chip allows.
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let hw = config::Board::take(peripherals);

    let timg0 = TimerGroup::new(hw.timg0);
    esp_rtos::start(timg0.timer0);

    // Bring up the APP_CPU with its own esp-rtos scheduler.  The closure
    // creates a thread-mode embassy executor on the second core and spawns
    // the solver task there.
    let software_interrupts = SoftwareInterruptControl::new(hw.sw_interrupt);
    let sha = hw.sha;
    esp_rtos::start_second_core(
        hw.cpu_ctrl,
        software_interrupts.software_interrupt0,
        software_interrupts.software_interrupt1,
        APP_CORE_STACK.init(Stack::new()),
        move || {
            let executor = APP_EXECUTOR.init(esp_rtos::embassy::Executor::new());
            executor.run(|spawner| {
                spawner.must_spawn(solver_task(sha));
            });
        },
    );

    let uart_config = UartConfig::default().with_baudrate(BAUD_RATE);
    // esp-hal 1.0.0 ships no UART DMA driver; the interrupt+FIFO
    // `into_async()` path here is the full async story in this release.
    // Issues esp-rs/esp-hal#2642 (UART DMA) and #4720 (circular UHCI RX)
    // closed after 1.0.0 and would unlock a streaming-DMA backend;
    // revisit when the next esp-hal release reaches this workspace.
    // The S3's 128-byte hardware FIFO plus the reader running on
    // PRO_CPU absorbs the 921 600-baud stream comfortably.
    // Valid UART peripheral and fixed baud rate; this cannot fail.
    let uart = Uart::new(hw.uart1, uart_config)
        .expect("BUG: UART configuration failed")
        .with_tx(hw.uart_tx)
        .with_rx(hw.uart_rx)
        .into_async();

    let (uart_rx, uart_tx) = uart.split();
    spawner.must_spawn(reader_task(uart_rx));
    spawner.must_spawn(writer_task(uart_tx));
}
