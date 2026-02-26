//! Single-consumer channel patterns.
//!
//! This module provides topology-first names for single-consumer channels.
//!
//! - `spsc`: single-publisher, single-consumer
//! - `mpsc`: multi-publisher, single-consumer

/// Single-publisher, single-consumer channel.
pub mod spsc;

/// Multi-publisher, single-consumer channel.
pub mod mpsc;
