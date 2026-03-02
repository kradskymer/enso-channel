#![allow(clippy::type_complexity)]
//! enso_channel
//!
//! **Bounded. Lock-free. Batch-native.**
//!
//! `enso_channel` is a batch-first concurrency primitive: a family of bounded, lock-free,
//! ring-buffer channels designed for bursty, latency-sensitive systems.
//!
//! The API is intentionally non-blocking: operations are exposed as `try_*` and surface
//! backpressure/termination explicitly via errors (`Full`, `Empty`, `Disconnected`).
//!
//! ## Mental model
//!
//! Instead of sending items one-by-one, producers typically:
//!
//! 1. claim a contiguous range in the ring buffer,
//! 2. write into it,
//! 3. commit the range.
//!
//! Receivers observe items via RAII guards/iterators; dropping them commits consumption.
//!
//! ## Public API modules
//!
//! - [`mpsc`]: multi-producer, single-consumer
//! - [`broadcast`]: lossless fixed-N fanout (each receiver sees every item)
//! - [`mpmc`]: multi-producer, multi-consumer work distribution
//!
//! ## Non-goals
//!
//! - no blocking API
//! - no async/await integration
//! - no dynamic resizing
//! - no built-in scheduling policy
//!
//! ## Lifecycle / shutdown (RAII)
//!
//! This crate intentionally does **not** expose an explicit `close()`/`terminate()` API.
//! Shutdown is expressed through normal Rust endpoint lifecycle:
//!
//! - dropping the last sender initiates shutdown; receivers may drain already-committed items
//!   and then observe `Disconnected`;
//! - dropping the last receiver disconnects senders (subsequent sends return `Disconnected`).
//!
//! ### Concurrency caveat
//!
//! Disconnection is **eventual, not transactional**.
//! In concurrent code, an operation may still succeed while the peer endpoint is being dropped,
//! and already-committed items may never be observed by the application.

#[macro_use]
mod channel_api_macros;

pub mod broadcast;
pub mod mpmc;
pub mod mpsc;

mod ringbuffer;
mod slot_states;

mod consumers;
mod publisher;

mod sequencers;

pub(crate) mod permit;

pub mod errors;
mod sequence;

mod cursor;

pub(crate) use cursor::Cursor;
pub(crate) use sequence::Sequence;
pub(crate) use sequencers::{ConsumerSeqGate, PublisherSeqGate};

pub(crate) use ringbuffer::{RingBuffer, RingBufferMeta};

#[cfg(test)]
mod send_sync_tests {
    //! Compile-time tests to ensure all channel types implement Send.
    //!
    //! These tests verify the thread-safety contract for this crate:
    //! all Sender and Receiver types must be transferable between threads.

    fn assert_send<T: Send>() {}

    #[allow(dead_code)]
    fn assert_sync<T: Sync>() {}

    #[test]
    fn mpsc_is_send() {
        assert_send::<crate::mpsc::Sender<u32>>();
        assert_send::<crate::mpsc::Receiver<u32>>();
    }

    #[test]
    fn broadcast_is_send() {
        assert_send::<crate::broadcast::Sender<u32, 2>>();
        assert_send::<crate::broadcast::Receiver<u32>>();
    }

    #[test]
    fn mpmc_is_send() {
        assert_send::<crate::mpmc::Sender<u32>>();
        assert_send::<crate::mpmc::Receiver<u32>>();
    }
}
