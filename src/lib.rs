#![allow(clippy::type_complexity)]
//! enso_channel: High-performance ring-buffer based channels.
//!
//! Public API modules:
//! - `mpsc`: multi-producer, single-consumer
//! - `broadcast`: lossless fixed-N broadcast (each receiver sees every item)
//! - `mpmc`: multi-producer, multi-consumer work queue
//!
//! Internal topology modules remain private implementation details.
//!
//! ## Lifecycle / shutdown
//!
//! This crate intentionally does **not** expose an explicit `close()`/`terminate()` API.
//! Shutdown is expressed through normal Rust endpoint lifecycle (RAII), similar to
//!  std mpsc channels:
//!
//! - dropping the last sender initiates shutdown; receivers may drain already-committed items
//!   and then observe `Disconnected` errors;
//! - dropping the last receiver disconnects senders (subsequent sends return `Disconnected`).
//!
//! Shutdown coordination is implemented by internal sequencers and shared gates.

#[macro_use]
mod channel_api_macros;

pub mod mpsc;
pub mod broadcast;
pub mod mpmc;

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
