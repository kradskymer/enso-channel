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
//! ## Misuse prevention (compile-time)
//!
//! Batch receives return a guard that commits consumption on drop. To keep this sound,
//! the guard is intentionally *not* an `Iterator<Item = &T>`.
//!
//! ```compile_fail
//! use enso_channel::mpsc;
//! # fn main() {
//! let (mut tx, mut rx) = mpsc::channel::<u64>(4);
//! tx.try_send(1).unwrap();
//! let batch = rx.try_recv_at_most(1).unwrap();
//! // `RecvIter` is not an iterator; use `batch.iter()` instead.
//! for v in batch {
//!     let _ = v;
//! }
//! # }
//! ```
//!
//! References yielded by `batch.iter()` are tied to the borrow of the batch guard and
//! cannot outlive it:
//!
//! ```compile_fail
//! use enso_channel::mpsc;
//! # fn main() {
//! let (mut tx, mut rx) = mpsc::channel::<u64>(4);
//! tx.try_send(1).unwrap();
//!
//! let r: &u64 = {
//!     let batch = rx.try_recv_at_most(1).unwrap();
//!     batch.iter().next().unwrap()
//! };
//! let _ = *r;
//! # }
//! ```
//!
//! And you can't commit (drop/`finish`) the guard while holding a reference from it:
//!
//! ```compile_fail
//! use enso_channel::mpsc;
//! # fn main() {
//! let (mut tx, mut rx) = mpsc::channel::<u64>(4);
//! tx.try_send(1).unwrap();
//!
//! let batch = rx.try_recv_at_most(1).unwrap();
//! let first = batch.iter().next().unwrap();
//! batch.finish();
//! let _ = *first;
//! # }
//! ```
//!
//! ## Public API modules
//!
//! - [`mpsc`]: multi-producer, single-consumer
//! - [`fanout`]: lossless fixed-N fanout (each receiver sees every item)
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

mod cursor;
mod receiver;
mod ringbuffer;
mod sender;
mod sequence;
mod sequencers;
mod slot_states;

mod guards;

pub(crate) use cursor::Cursor;
pub(crate) use ringbuffer::{RingBuffer, RingBufferMeta};
pub(crate) use sequence::Sequence;
pub(crate) use sequencers::ProducerBarrier;

pub use guards::{ChanReadRef, ChanReadRefs, ChanWritePermit, ChanWritePermits};
pub use receiver::ChanReceiver;
pub use sender::ChannelSender;
pub use slot_recycler::SlotRecycler;

pub mod errors;
pub mod fanout;
pub mod mpsc;
pub mod slot_recycler;

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
        assert_send::<crate::fanout::Sender<2, u32>>();
        assert_send::<crate::fanout::Receiver<u32>>();
    }
}
