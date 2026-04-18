//! Firmware entry point for the W6100-EVB-Pico-2. Initialises SPI at
//! 33 MHz, brings the W6100 up via `embassy-net-wiznet`, starts the
//! embassy-net stack with DHCP, and spawns the watchdog, LED blink,
//! ethernet runner, NTP sync, and BTC price tasks before entering
//! the HTTP server loop. Built with `cargo build` from this crate
//! inside `nix develop`.

#![no_std]
#![no_main]

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_net_wiznet::chip::W6100;
use embassy_net_wiznet::{Device, Runner, State};
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, SPI0, UART1};
use embassy_rp::spi::{Async, Config as SpiConfig, Spi};
use embassy_rp::uart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_rp::watchdog::Watchdog;
use embassy_rp::{bind_interrupts, dma};
use embassy_time::Delay;
use embedded_hal_bus::spi::ExclusiveDevice;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use eth_examples::net;

mod config;

defmt::timestamp!("{=u64:iso8601s}", eth_examples::wall_clock_secs());

bind_interrupts!(struct Irqs {
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>;
    UART1_IRQ => BufferedInterruptHandler<UART1>;
});

type EthernetRunner = Runner<
    'static,
    W6100,
    ExclusiveDevice<Spi<'static, SPI0, Async>, Output<'static>, Delay>,
    Input<'static>,
    Output<'static>,
>;

struct BoardWatchdog(Watchdog);

impl eth_examples::watchdog::Watchdog for BoardWatchdog {
    fn init(&mut self) {
        self.0.start(config::Watchdog::TIMEOUT);
    }

    fn feed(&mut self) {
        self.0.feed(config::Watchdog::TIMEOUT);
    }
}

#[embassy_executor::task]
async fn ethernet_task(runner: EthernetRunner) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device<'static>>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn watchdog_task(watchdog: BoardWatchdog) -> ! {
    eth_examples::watchdog::run_watchdog(watchdog).await
}

#[embassy_executor::task]
async fn led_blink_task(led: Output<'static>) -> ! {
    eth_examples::led::run_led_blink(led).await
}

#[embassy_executor::task]
async fn bitcoin_price_task(stack: embassy_net::Stack<'static>, rng: RoscRng) -> ! {
    net::run_bitcoin_price(stack, rng).await
}

#[embassy_executor::task]
async fn ntp_sync_task(stack: embassy_net::Stack<'static>) -> ! {
    net::run_ntp_sync(stack).await
}

#[embassy_executor::task]
async fn uart_echo_test(mut uart: BufferedUart) -> ! {
    let _ = eth_examples::uart_echo::run_test(&mut uart).await;
    core::future::pending::<()>().await;
    unreachable!()
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let peripherals = embassy_rp::init(embassy_rp::config::Config::default());
    let hw = config::Board::take(peripherals);
    let mut rng = RoscRng;

    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(Watchdog::new(
        hw.watchdog
    )))));

    let led = Output::new(hw.led, Level::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    let mut spi_config = SpiConfig::default();
    spi_config.frequency = config::Spi::FREQUENCY;
    let spi = Spi::new(
        hw.spi, hw.sck, hw.mosi, hw.miso, hw.rx_dma, hw.tx_dma, Irqs, spi_config,
    );
    let chip_select = Output::new(hw.cs, Level::High);
    let w6100_int = Input::new(hw.w6100_int, Pull::Up);
    let w6100_reset = Output::new(hw.w6100_reset, Level::High);

    let mac_addr = config::Board::MAC_ADDR;
    static STATE: StaticCell<State<8, 8>> = StaticCell::new();
    let state = STATE.init(State::<8, 8>::new());
    let (device, runner) = embassy_net_wiznet::new(
        mac_addr,
        state,
        ExclusiveDevice::new(spi, chip_select, Delay),
        w6100_int,
        w6100_reset,
    )
    .await
    // Initialization succeeds when the chip is wired correctly and responds on SPI.
    .expect("BUG: W6100 initialization failed");
    spawner.spawn(unwrap!(ethernet_task(runner)));

    let seed = rng.next_u64();

    static RESOURCES: StaticCell<StackResources<6>> = StaticCell::new();
    let (stack, runner) = embassy_net::new(
        device,
        embassy_net::Config::dhcpv4(embassy_net::DhcpConfig::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(unwrap!(net_task(runner)));

    spawner.spawn(unwrap!(bitcoin_price_task(stack, rng)));
    spawner.spawn(unwrap!(ntp_sync_task(stack)));

    static TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
    let uart = BufferedUart::new(
        hw.uart,
        hw.uart_tx,
        hw.uart_rx,
        Irqs,
        TX_BUF.init([0; 256]),
        RX_BUF.init([0; 256]),
        {
            let mut config = UartConfig::default();
            config.baudrate = eth_examples::uart_echo::BAUD_RATE;
            config
        },
    );
    spawner.spawn(unwrap!(uart_echo_test(uart)));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", config.address.address());

    net::run_http_server(stack, config::Board::NAME).await;
}
