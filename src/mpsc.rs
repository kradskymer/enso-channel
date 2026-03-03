//! Multi-producer, single-consumer (MPSC) channel.
//!
//! This module provides a bounded MPSC channel backed by a ring buffer.
//!
//! Key properties:
//! - **Bounded**: capacity is fixed at creation time.
//! - **Non-blocking**: operations are `try_*` and return an error on full/empty.
//! - **Cloneable publishers**: `Sender<T>` is `Clone`.
//! - **Zero-copy receive**: receives yield `&T` via guard/iterator types that
//!   commit consumption on drop.
//!
//! # Capacity
//!
//! `capacity` must be a power of two.
//!
//! # Examples
//!
//! ```
//! use enso_channel::mpsc;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (mut tx, mut rx) = mpsc::channel::<u64>(64);
//!
//! tx.try_send(42)?;
//! let _v = *rx.try_recv()?;
//!
//! let mut batch = tx.try_send_many_default(8)?;
//! batch.fill_with(|| 1);
//! batch.finish();
//!
//! let iter = rx.try_recv_at_most(8)?;
//! for _ in iter {
//!     // ...
//! }
//! # Ok(()) }
//! ```

use std::sync::Arc;

use crate::sequencers::{ExclusiveConSeqGate, ExclusiveConsumerSequencer};
use crate::sequencers::{MultiPubSeqGate, MultiPublisherSequencer};
use crate::RingBuffer;

type PublisherSequencer = MultiPublisherSequencer<ExclusiveConSeqGate>;
type ConsumerSequencer = ExclusiveConsumerSequencer<MultiPubSeqGate>;

type Publisher<T> = crate::publisher::Publisher<PublisherSequencer, T>;
type Consumer<T> = crate::consumers::Consumer<ConsumerSequencer, T>;

/// Creates a bounded MPSC channel using `T::default()` to initialize the ring.
///
/// `capacity` must be a power of two.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>)
where
    T: Default,
{
    let ring_buffer = Arc::new(RingBuffer::init_with_default(capacity));
    channel_with_ring(ring_buffer)
}

/// Creates a bounded MPSC channel, initializing the ring buffer with `initializer`.
///
/// `capacity` must be a power of two.
///
/// `initializer` is invoked once per slot index during pre-allocation.
/// The bound `Copy + FnOnce(usize) -> T` allows passing non-capturing closures and
/// function pointers while still calling the initializer multiple times.
pub fn channel_with<T, I>(capacity: usize, initializer: I) -> (Sender<T>, Receiver<T>)
where
    I: Copy + FnOnce(usize) -> T,
{
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    channel_with_ring(ring_buffer)
}

fn channel_with_ring<T>(ring_buffer: Arc<RingBuffer<T>>) -> (Sender<T>, Receiver<T>) {
    let capacity = ring_buffer.capacity() as usize;

    // Shared consumed cursor used by the consumer gate and the consumer sequencer.
    let consumed = Arc::new(crate::Cursor::new(crate::Sequence::INIT));
    let consumer_gate = Arc::new(ExclusiveConSeqGate::new(consumed.clone()));

    let ring_meta = crate::RingBufferMeta::new(capacity);
    let publisher_sequencer = PublisherSequencer::new(consumer_gate, ring_meta);
    let publisher_gate = Arc::new(publisher_sequencer.publisher_gate());

    let consumer_sequencer = ConsumerSequencer::new(consumed, publisher_gate, ring_meta);

    let sender = Sender {
        inner: Publisher::new(publisher_sequencer, ring_buffer.clone()),
    };

    let receiver = Receiver {
        inner: Consumer::new(consumer_sequencer, ring_buffer),
    };

    (sender, receiver)
}

channel_define_sender! {
    /// The sending half of an MPSC channel.
    ///
    /// This type is `Clone`.
    #[derive(Clone)]
    pub struct Sender<T> {
        inner: Publisher<T>,
    }
    => SendBatch = SendBatch;
}

channel_define_send_batch! {
    pub struct SendBatch<'a, T, F> = (PublisherSequencer);
}

channel_define_receiver! {
    /// The receiving half of an MPSC channel.
    pub struct Receiver<T> {
        inner: Consumer<T>,
    }
    => RecvGuard = RecvGuard, RecvIter = RecvIter;
}

channel_define_recv_guard! {
    pub struct RecvGuard<'a, T> = (ConsumerSequencer);
}

channel_define_recv_iter! {
    pub struct RecvIter<'a, T> = (ConsumerSequencer);
}
