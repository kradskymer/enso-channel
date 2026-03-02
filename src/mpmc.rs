//! Multi-producer, multi-consumer (MPMC) work distribution channel.
//!
//! This topology distributes items across a pool of receivers.
//! [`Receiver<T>`] is [`Clone`], so you can spawn multiple workers that compete for work.
//!
//! Like other topologies in this crate, it is bounded, non-blocking, and batch-native:
//! operations are `try_*` and surface explicit backpressure.
//!
//! # Capacity
//!
//! `capacity` must be a power of two.
//!
//! # Examples
//!
//! ```
//! use enso_channel::mpmc;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (mut tx, mut rx1) = mpmc::channel::<u64>(64);
//! let mut rx2 = rx1.clone();
//!
//! tx.try_send(1)?;
//! let _v = *rx1.try_recv()?;
//! let _ = rx2.try_recv_at_most(8); // competing worker
//! # Ok(()) }
//! ```

use std::sync::Arc;

use crate::RingBuffer;
use crate::sequencers::{
    MultiPubSeqGate, MultiPublisherSequencer, QueuedConSeqGate, QueuedConsumerSequencer,
    QueuedConsumerWiring,
};

type PublisherSequencer = MultiPublisherSequencer<QueuedConSeqGate>;
type ConsumerSequencer = QueuedConsumerSequencer<MultiPubSeqGate>;

type Publisher<T> = crate::publisher::Publisher<PublisherSequencer, T>;
type Consumer<T> = crate::consumers::Consumer<ConsumerSequencer, T>;

/// Creates a bounded MPMC work distribution channel.
///
/// `capacity` must be a power of two.
///
/// The ring buffer is pre-allocated and initialized with `T::default()`.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>)
where
    T: Default,
{
    let ring_buffer = Arc::new(RingBuffer::init_with_default(capacity));
    channel_with_ring(ring_buffer)
}

/// Creates a bounded MPMC work distribution channel, initializing the ring buffer with `initializer`.
///
/// `capacity` must be a power of two.
///
/// `initializer` is invoked once per slot index during pre-allocation.
/// The bound `Copy + FnOnce(usize) -> T` allows passing non-capturing closures and function pointers while still calling the initializer multiple times.
pub fn channel_with<T, I>(capacity: usize, initializer: I) -> (Sender<T>, Receiver<T>)
where
    I: Copy + FnOnce(usize) -> T,
{
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    channel_with_ring(ring_buffer)
}

fn channel_with_ring<T>(ring_buffer: Arc<RingBuffer<T>>) -> (Sender<T>, Receiver<T>) {
    let capacity = ring_buffer.capacity() as usize;
    let ring_meta = crate::RingBufferMeta::new(capacity);

    let (consumer_gate, consumer_wiring) = QueuedConsumerWiring::new(ring_meta);

    let publisher_sequencer = PublisherSequencer::new(consumer_gate, ring_meta);
    let publisher_gate = Arc::new(publisher_sequencer.publisher_gate());

    let consumer_sequencer = consumer_wiring.build_consumer(publisher_gate, ring_meta);

    let sender = Sender {
        inner: Publisher::new(publisher_sequencer, ring_buffer.clone()),
    };
    let receiver = Receiver {
        inner: Consumer::new(consumer_sequencer, ring_buffer),
    };

    (sender, receiver)
}

channel_define_sender! {
    /// The sending half of a work-stealing MPMC channel.
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
    /// The receiving half of a work-stealing channel.
    ///
    /// This type is `Clone` to allow spawning multiple workers.
    #[derive(Clone)]
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
