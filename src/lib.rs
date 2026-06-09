//! ## What is enso-channel?
//!
//! `enso_channel` is a batch-first concurrency primitive: a family of bounded, lock-free,
//! ring-buffer channels designed for bursty, latency-sensitive systems.
//!
//! `enso-channel` explores a **batch-first design space** for lock-free ring-buffer channels
//! in Rust.
//!
//! The API is intentionally non-blocking: operations are exposed as `try_*` and surface
//! backpressure/termination explicitly via errors (`Full`, `Empty`, `Disconnected`).
//!
//!
//! ### Built for systems where
//!
//! - Burst traffic is common
//! - Tail latency (p99 / p99.9) matters
//! - Back pressure must be explicit
//! - Memory is bounded and pre-allocated
//! - Scheduling is handled at a higher layer
//!
//! > **Note:** If you need blocking APIs, dynamic resizing, or async senders — this crate is
//! > probably not for you. Receivers support async via an opt-in feature flag
//! > (`async-receiver`).
//!
//! ## Design Philosophy
//!
//! `enso-channel` is a **concurrency primitive**, not a runtime.
//!
//! ### Core Principles
//!
//! - **Lock-free**
//! - **Bounded capacity**
//! - **Pre-allocated memory**
//! - **Explicit back pressure**
//! - **Native batch claim/commit**
//! - **No hidden scheduling**
//! - **No implicit blocking**
//! - **Asymmetric async (receiver only, opt-in)**
//!
//! ### Mental Model
//!
//! Instead of sending / receiving items one-by-one:
//!
//! ```text
//! 1. Claim a contiguous range in the ring buffer
//! 2. Write into it / Read from it
//! 3. Commit the range
//! ```
//!
//! **Batching amortizes synchronization cost and reduces tail latency under burst workloads.**
//!
//! #### Inspired by
//!
//! - [LMAX Exchange's Disruptor](https://github.com/LMAX-Exchange/disruptor)
//! - [crossbeam-channel](https://github.com/crossbeam-rs/crossbeam)
//!
//! #### But with
//!
//! - Rust-native channel ergonomics
//! - Unified API across topologies
//! - Clear primitive boundaries
//!
//! ## Misuse prevention (compile-time)
//!
//! Batch receives return a guard that commits consumption on drop. To keep this sound,
//! the guard is intentionally *not* an `Iterator<Item = &T>`.
//!
//! References yielded by `batch.iter()` are tied to the borrow of the batch guard and
//! cannot outlive it:
//!
//! ```compile_fail
//! use enso_channel::mpsc;
//! # use enso_channel::prelude::*;
//! let (mut tx, mut rx) = mpsc::channel::<u64>(4);
//! tx.try_send(1).unwrap();
//!
//! let r: &u64 = {
//!     let batch = rx.try_recv_at_most(1).unwrap();
//!     batch.iter().next().unwrap()
//! };
//! let _ = *r;
//! ```
//!
//! And you can't commit (drop/`finish`) the guard while holding a reference from it:
//!
//! ```compile_fail
//! use enso_channel::mpsc;
//! # use enso_channel::prelude::*;
//! let (mut tx, mut rx) = mpsc::channel::<u64>(4);
//! tx.try_send(1).unwrap();
//!
//! let batch = rx.try_recv_at_most(1).unwrap();
//! let first = batch.iter().next().unwrap();
//! batch.finish();
//! let _ = *first;
//! ```
//!
//! ## Public API modules
//!
//! - [`mpsc`]: multi-producer, single-consumer
//! - [`fanout`]: lossless fixed-N fanout (each receiver sees every item)
//!
//! ## Non-goals
//!
//! > To prevent misuse and ambiguity:
//!
//! - No blocking API
//! - No dynamic resizing
//! - No built-in scheduling policy
//! - No priority queues
//! - No fairness guarantees beyond lock-free progress
//! - No **async senders** (see [Async model](#async-model-receiver-only))
//!
//! **Philosophy:** Scheduling, wake-up strategy, budgeting, priority, and flow control belong
//! to higher layers.
//!
//! `enso-channel` intentionally surfaces `Full`, `Empty`, and `Disconnected` instead of hiding
//! them behind blocking semantics.
//!
//! The **receiver** side offers optional async support (`recv_async`, `recv_at_most_async`)
//! behind the `async-receiver` feature flag. The **sender** side remains purely non-blocking
//! (`try_*` only).
//!
//! ## Async model (receiver only)
//!
//! `enso-channel` takes an **asymmetric approach to async**: only the receiver side supports
//! async/await.
//!
//! This design rests on two observations:
//!
//! 1. **Senders are upstream-driven, receivers are sender-driven.**
//!    In most systems, senders are paced by external input (IO, timers, other actors).
//!    They don't naturally "wait" — they push when data arrives.
//!    Receivers, by contrast, often have nothing to do until senders produce, making
//!    async a natural fit for the consumer side.
//!
//! 2. **Cloneable senders make waker coordination expensive.**
//!    Every sender type in `enso-channel` is `Clone`.
//!    Adding wakers to the sender side would require synchronising across an arbitrary
//!    number of cloned handles (mutex, atomic-set, or similar), introducing contention
//!    at every send. That directly conflicts with the crate's low-latency, lock-free
//!    goals — especially when senders rarely need to block in practice.
//!
//! ### Enabling async
//!
//! ```sh
//! cargo add enso-channel --features async-receiver
//! ```
//!
//! ```rust,ignore
//! use enso_channel::mpsc;
//! use enso_channel::prelude::*;
//!
//! async fn example(mut rx: mpsc::Receiver<u64>) {
//!     // Wait for a single item
//!     let guard = rx.recv_async().await.unwrap();
//!     println!("{}", *guard);
//!
//!     // Wait for a batch of up to 8 items
//!     let batch = rx.recv_at_most_async(8).await.unwrap();
//!     for item in batch.iter() {
//!         println!("{}", item);
//!     }
//! }
//! ```
//!
//! ### How it works
//!
//! - Producers notify receivers each time they commit published data (via
//!   `commit` / `commit_range`).
//! - The receiver registers its waker with a lock-free `AtomicWaker`, and the next commit
//!   wakes it.
//! - Shutdown (drop of the last sender) also triggers a wake, causing `recv_async` to
//!   return `None`.
//!
//! This keeps the sender fast-path entirely free of waker overhead — no lists to scan, no
//! locks to acquire. The only added cost on the producer side is a single
//! `AtomicWaker::wake()` call per commit when the `async-receiver` feature is active (zero
//! cost when disabled).
//!
//! ## Communication Patterns
//!
//! Unified API across multiple topologies:
//!
//! | Pattern     | Description                         |
//! |-------------|-------------------------------------|
//! | **MPSC**    | Multiple Producer, Single Consumer  |
//! | **Fan-out** | Lossless Broadcast (fixed‑N fanout) |
//!
//! ### Key Properties
//!
//! - Receivers may initiate disconnect
//! - Broadcast topology is fixed at creation
//! - Disconnection follows RAII semantics
//! - All patterns support batch operations
//!
//! ## System-Wide Tunability
//!
//! Tunability in `enso-channel` is not limited to a specific topology.
//!
//! It emerges from its **batch-native design**.
//!
//! ### Every pattern supports
//!
//! | API                                     | Description              |
//! |-----------------------------------------|--------------------------|
//! | `try_send_at_most` / `try_recv_at_most` | At-most semantics        |
//! | Explicit backpressure                   | Surface `Full` / `Empty` |
//!
//! ### This enables systematic control over
//!
//! - **Contention levels**
//! - **Cache locality**
//! - **Burst amortization**
//! - **Producer/consumer balance**
//! - **Tail latency behavior**
//!
//! ## Getting Started
//!
//! ### Installation
//!
//! ```sh
//! cargo add enso-channel
//! ```
//!
//! ### Example (MPSC)
//!
//! ```rust
//! use enso_channel::mpsc;
//! use enso_channel::prelude::*;
//! use enso_channel::slot_recycler::ResetWithDefault;
//!
//! fn main() {
//!     let (mut tx, mut rx) = mpsc::channel::<u64>(64).unwrap();
//!
//!     // Single send/recv
//!     tx.try_send(42).unwrap();
//!     {
//!         let guard = rx.try_recv().unwrap();
//!         assert_eq!(*guard, 42);
//!     }
//!
//!     // Batch send
//!     let mut batch = tx.try_send_at_most(8, ResetWithDefault).unwrap();
//!     for i in 1..=8 {
//!         batch.next().unwrap().write(i);
//!     }
//!     batch.commit();
//!
//!     // Batch receive
//!     let guard = rx.try_recv_at_most(8).unwrap();
//!     for v in guard.iter() {
//!         println!("{v}");
//!     }
//! }
//! ```
//!
//! **All topologies share a consistent API shape.**
//!
//! See [documentation](https://docs.rs/enso-channel) for additional examples.
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
//! ### Disconnection follows RAII
//!
//! - Dropping the last sender disconnects receivers (after draining committed items)
//! - Dropping the last receiver disconnects senders
//! - Batch guards commit automatically on drop
//!
//! ### Important caveat
//!
//! > **Disconnection is eventual, not transactional.**
//!
//! A sender may still successfully publish if a receiver is dropped concurrently.
//! Already-committed items may never be observed by the application.
//!
//! For example:
//!
//! 1. A sender reserves a permit / batch permit, and the receiver(s) disconnect afterwards.
//! 2. A sender sends an item successfully, but the receiver(s) disconnect without
//!    consumption.
//!
//! ### Panic safety: poison shutdown
//!
//! The two-phase commit pattern (claim → write → commit) depends on
//! [`SlotRecycler`] as the fallback for unwritten slots.
//! **Your `SlotRecycler` must never panic** — it is the last defense for repairing
//! stale claims.
//!
//! If a `SlotRecycler` does panic, the channel contract is broken (claimed slots can
//! no longer be repaired). The crate responds by **shutting down the entire channel**
//! to prevent undefined behaviour.
//!
//! Only **step 1 (claim)** checks for shutdown:
//!
//! | Step              | Shutdown checked? | Rationale                                    |
//! |-------------------|:-----------------:|----------------------------------------------|
//! | 1. Claim slots    | yes               | natural gate; `Disconnected` returned here   |
//! | 2. Write data     | no                | per-slot checks would hurt the hot path      |
//! | 3. Commit         | no                | data already written; late check can't help  |
//!
//! In practice, this means a sender that committed data *after* another sender
//! panicked (but before its own next claim) will see that data silently discarded.
//! Published items committed *before* the panic remain drainable by receivers, up to
//! the shutdown boundary.
//!
//! ### Graceful shutdown without loss
//!
//! ```text
//! 1. Stop producing
//! 2. Drop all senders
//! 3. Drain on the receiver until `Disconnected`
//! ```

mod cursor;
mod receiver;
mod ringbuffer;
mod sender;
mod sequence;
mod sequencers;
mod slot_states;
mod waker;

mod guards;

pub(crate) use cursor::Cursor;
pub(crate) use ringbuffer::{RingBuffer, RingBufferMeta};
pub(crate) use sequence::Sequence;
pub(crate) use sequencers::ProducerBarrier;

pub use guards::{ChanReadRef, ChanReadRefs, ChanWritePermit, ChanWritePermits};
pub use receiver::ChanReceiver;
pub use sender::ChanSender;
pub use slot_recycler::SlotRecycler;

pub mod errors;
pub mod fanout;
pub mod mpsc;
pub mod slot_recycler;

pub mod prelude {
    pub use crate::guards::{ChanReadRef, ChanReadRefs, ChanWritePermit, ChanWritePermits};
    pub use crate::receiver::ChanReceiver;
    pub use crate::sender::ChanSender;
    pub use crate::slot_recycler::SlotRecycler;
}

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
