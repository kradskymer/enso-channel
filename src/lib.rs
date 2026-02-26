#![allow(clippy::type_complexity)]
//! enso_channel: High-performance ring-buffer based channels.
//!
//! This crate organizes channels by **topology** (how many producers/consumers)
//! and by **semantics** (fanout vs work distribution, lossless vs lossy).
//!
//! ## Semantics table
//!
//! | Module | Variants | Delivery semantics | Publisher back pressure | Consumer cloning |
//! | --- | --- | --- | --- | --- | --- |
//! | `exclusive` | `spsc`, `mpsc` | Each item is received by exactly **one** consumer (single-consumer). | **Lossless**; bounded by ring capacity and consumer progress. | `spsc` receiver not cloneable; `mpsc` sender cloneable. |
//! | `fanout` | `spmc`, `mpmc` | **Fanout, lossless**: each consumer is expected to observe every item. | **Gated by the slowest consumer** (LMAX/Disruptor-style `min(consumed)` gating). | Consumers are fixed (no dynamic subscribe). |
//! | `queue` | `spmc`, `mpmc` | **Work distribution**: each item is processed by exactly **one** worker; workers compete to claim items. | Bounded by ring capacity; should not depend on the slowest worker cursor. | Receivers are cloneable (worker pool). |
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

pub mod exclusive;
pub mod fanout;
pub mod queue;

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
    fn exclusive_spsc_is_send() {
        assert_send::<crate::exclusive::spsc::Sender<u32>>();
        assert_send::<crate::exclusive::spsc::Receiver<u32>>();
    }

    #[test]
    fn exclusive_mpsc_is_send() {
        assert_send::<crate::exclusive::mpsc::Sender<u32>>();
        assert_send::<crate::exclusive::mpsc::Receiver<u32>>();
    }

    #[test]
    fn fanout_spmc_is_send() {
        assert_send::<crate::fanout::spmc::Sender<u32, 2>>();
        assert_send::<crate::fanout::spmc::Receiver<u32>>();
    }

    #[test]
    fn fanout_mpmc_is_send() {
        assert_send::<crate::fanout::mpmc::Sender<u32, 2>>();
        assert_send::<crate::fanout::mpmc::Receiver<u32>>();
    }

    #[test]
    fn queue_spmc_is_send() {
        assert_send::<crate::queue::spmc::Sender<u32>>();
        assert_send::<crate::queue::spmc::Receiver<u32>>();
    }

    #[test]
    fn queue_mpmc_is_send() {
        assert_send::<crate::queue::mpmc::Sender<u32>>();
        assert_send::<crate::queue::mpmc::Receiver<u32>>();
    }
}
