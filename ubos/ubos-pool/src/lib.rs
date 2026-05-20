//! Pool-side configuration and external price ticker parsing.
//!
//! `pool_config` resolves the compile-time `POOL_*` environment
//! overrides into a typed `PoolConfig` that drives the runtime
//! protocol selector in the stratum task. `btc` extracts the BTC
//! price from a Binance ticker JSON response and is consumed by the
//! firmware's BTC fetch task.
#![no_std]

pub mod btc;
pub mod pool_config;
