//! Fixed-fanout (LMAX/Disruptor-style) channels.
//!
//! Shared channels have multiple consumers that each observe every published
//! item (fan-out), with **publisher gating** based on the *minimum* consumed
//! sequence across all consumers.
//!
//! Design constraints (agreed):
//! - Consumers are fixed at build time (no dynamic add/remove).
//! - A slow consumer blocks publishers (LMAX-style gating).
//! - Maximum consumers is small (target max: 8).

pub mod mpmc;
pub mod spmc;
