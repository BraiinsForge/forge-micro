//! Hardware reset for the BM1362 chip chain.
//!
//! Active-low on the BMM100: backends drive the pin LOW to assert
//! reset and HIGH to release it. Methods are infallible — a failed
//! GPIO toggle is unrecoverable from the caller's perspective, so
//! the convention is "log internally and proceed"; the next chip
//! operation will surface the underlying issue.

pub trait ResetPin {
    /// Hold the chip in reset (drive the line LOW).
    fn enter_reset(&mut self);

    /// Release the chip from reset (drive the line HIGH).
    fn exit_reset(&mut self);
}
