//! Hardware reset for the BM1362 chip chain.
//!
//! Active-low on the BMM100: backends drive the pin LOW to assert
//! reset and HIGH to release it. Methods are infallible — a failed
//! toggle is unrecoverable from the caller's perspective, so the
//! convention is "log internally and proceed"; the next chip
//! operation will surface the underlying issue.
//!
//! The methods are `async` because some backends drive the line
//! through an I2C I/O expander (the esp32s3 carrier's TCA9554 on
//! EXIO5), which needs an async bus transaction. GPIO-backed boards
//! implement them as plain non-awaiting bodies.

// async fn in a public trait: the only implementors are the per-board
// reset pins, consumed by `Miner` through a generic bound, never across
// an auto-trait boundary, so the Send-leakage caveat does not apply.
#[allow(async_fn_in_trait)]
pub trait ResetPin {
    /// Hold the chip in reset (drive the line LOW).
    async fn enter_reset(&mut self);

    /// Release the chip from reset (drive the line HIGH).
    async fn exit_reset(&mut self);
}
