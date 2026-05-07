//! Protocol-agnostic mining client trait and borrowed domain types.
//!
//! Defines the contract between protocol adapters (`stratum-v1`,
//! `stratum-v2`) and the integration layer (`stratum-client`). See
//! `boards/eth-examples/IMPLEMENTATION_PLAN.md` and
//! `boards/eth-examples/ARCHITECTURE.md` for the design.

#![no_std]
