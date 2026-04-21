//! Firmware entry point for the Nucleo-F439ZI board wired to a
//! Waveshare LAN8720 ETH Board via RMII. Starts the built-in MAC
//! with an auto-probed LAN8720 PHY, runs the embassy-net stack with
//! DHCP, and spawns the watchdog, LED blink, NTP, and BTC tasks
//! before entering the HTTP server loop. Built with `cargo build`
//! from this crate inside `nix develop .#arm`.

#![no_std]
#![no_main]

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::StackResources;
use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue, Sma};
use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::peripherals::{ETH, ETH_SMA, IWDG};
use embassy_stm32::rng::Rng;
use embassy_stm32::usart::{BufferedInterruptHandler, BufferedUart, Config as UartConfig};
use embassy_stm32::wdg::IndependentWatchdog;
use embassy_stm32::{bind_interrupts, eth, peripherals, rng};
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

use eth_examples::net;

mod config;

defmt::timestamp!("{=u64:iso8601s}", eth_examples::wall_clock_secs());

bind_interrupts!(struct Irqs {
    ETH => eth::InterruptHandler;
    HASH_RNG => rng::InterruptHandler<peripherals::RNG>;
    USART3 => BufferedInterruptHandler<peripherals::USART3>;
});

type Device = Ethernet<'static, ETH, GenericPhy<Sma<'static, ETH_SMA>>>;

struct BoardWatchdog(IndependentWatchdog<'static, IWDG>);

impl eth_examples::watchdog::Watchdog for BoardWatchdog {
    fn init(&mut self) {
        self.0.unleash();
    }

    fn feed(&mut self) {
        self.0.pet();
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, Device>) -> ! {
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
async fn bitcoin_price_task(
    stack: embassy_net::Stack<'static>,
    rng: Rng<'static, peripherals::RNG>,
) -> ! {
    net::run_bitcoin_price(stack, rng).await
}

#[embassy_executor::task]
async fn ntp_sync_task(stack: embassy_net::Stack<'static>) -> ! {
    net::run_ntp_sync(stack).await
}

#[embassy_executor::task]
async fn uart_echo_test(mut uart: BufferedUart<'static>) -> ! {
    let _ = eth_examples::uart_echo::run_test(&mut uart).await;
    core::future::pending::<()>().await;
    unreachable!()
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let peripherals = embassy_stm32::init(config::stm32_config());
    let hw = config::Board::take(peripherals);

    let watchdog = IndependentWatchdog::new(hw.iwdg, config::Watchdog::TIMEOUT_US);
    spawner.spawn(unwrap!(watchdog_task(BoardWatchdog(watchdog))));

    let led = Output::new(hw.led, Level::Low, Speed::Low);
    spawner.spawn(unwrap!(led_blink_task(led)));

    // Hardware RNG for DHCP and TCP sequence numbers.
    let mut rng = Rng::new(hw.rng, Irqs);
    let mut seed_bytes = [0u8; 8];
    // RNG peripheral is initialized and clocked via PLL; read cannot fail here.
    rng.async_fill_bytes(&mut seed_bytes)
        .await
        .expect("BUG: RNG fill failed");
    let seed = u64::from_le_bytes(seed_bytes);

    // RMII pin mapping for Nucleo-F439ZI + Waveshare LAN8720 ETH Board.
    static PACKETS: StaticCell<PacketQueue<4, 4>> = StaticCell::new();
    let device = Ethernet::new(
        PACKETS.init(PacketQueue::<4, 4>::new()),
        hw.eth,
        Irqs,
        hw.ref_clk,
        hw.crs_dv,
        hw.rxd0,
        hw.rxd1,
        hw.txd0,
        hw.txd1,
        hw.tx_en,
        config::Board::MAC_ADDR,
        hw.eth_sma,
        hw.mdio,
        hw.mdc,
    );

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
        hw.usart3,
        hw.usart3_rx,
        hw.usart3_tx,
        TX_BUF.init([0; 256]),
        RX_BUF.init([0; 256]),
        Irqs,
        {
            let mut config = UartConfig::default();
            config.baudrate = eth_examples::uart_echo::BAUD_RATE;
            config
        },
    )
    // Valid USART peripheral and default-compatible baud rate; cannot fail.
    .expect("BUG: USART3 configuration failed");
    spawner.spawn(unwrap!(uart_echo_test(uart)));

    info!("waiting for dhcp");
    let config = net::wait_for_config(stack).await;
    info!("ip address: {:?}", config.address.address());

    net::run_http_server(stack, config::Board::NAME).await;
}
