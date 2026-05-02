//! XIAO ESP32-S3 dummy ASIC: receives mining Work over UART, searches nonces
//! with the hardware SHA accelerator, returns Share packets with the found
//! nonce and hash.
//!
//! Embassy runs on both ESP32-S3 cores via `esp_rtos 0.2.0`:
//!   - Core 0 (`PRO_CPU`): UART reader + writer.
//!   - Core 1 (`APP_CPU`): solver task.
//!
//! The UART is split into `UartRx` / `UartTx` halves so the reader and writer
//! run concurrently without borrow contention.
//!
//! `esp_rtos::start_second_core` manages the `APP_CPU` scheduler.  The
//! thread-mode `esp_rtos::embassy::Executor` then runs on the second core,
//! started from within the closure passed to `start_second_core`.
//!
//! # Binary protocol — UART1, 921 600 baud, GPIO8 TX / GPIO9 RX
//!
//! ## Work frame (host → dummy-asic, opcode 0x01) — 119 bytes total
//!
//! | Offset | Size | Content |
//! |--------|------|---------|
//! | 0      | 1    | Start byte `0xA5` |
//! | 1      | 1    | Opcode `0x01` |
//! | 2      | 80   | 80-byte Bitcoin block header; the solver overwrites the nonce field |
//! | 82     | 32   | 256-bit target, little-endian; hash accepted when `digest <= target` |
//! | 114    | 4    | Host-assigned `work_id`, big-endian; echoed in every share derived from this Work |
//! | 118    | 1    | XOR-8 checksum over bytes 1–117 (opcode + payload) |
//!
//! ## Share frame (dummy-asic → host, opcode 0x02) — 43 bytes total
//!
//! | Offset | Size | Content |
//! |--------|------|---------|
//! | 0      | 1    | Start byte `0xA5` |
//! | 1      | 1    | Opcode `0x02` |
//! | 2      | 4    | Found nonce, big-endian |
//! | 6      | 4    | `work_id` of the Work that produced this share, big-endian |
//! | 10     | 32   | SHA-256d hash |
//! | 42     | 1    | XOR-8 checksum over bytes 1–41 (opcode + payload) |

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
