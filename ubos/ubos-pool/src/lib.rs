//! BTC price ticker parsing.
//!
//! `btc` extracts the BTC price from a Binance ticker JSON response
//! and is consumed by the firmware's BTC fetch task.
#![no_std]

pub mod btc;
