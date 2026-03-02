//! Multi-publisher, multi-consumer broadcast channel.
//!
//! This is the fixed-N, LMAX/Disruptor-style topology where each receiver
//! observes every published item and publishers are gated by the slowest
//! receiver.
//!
//! Dropping a receiver disconnects it and removes it from the gating set.
//! Once all receivers are dropped, send operations observe `Disconnected`.
//!
//! # Capacity
//!
//! `capacity` must be a power of two.
//!
//! # Examples
//!
//! ```
//! use enso_channel::broadcast;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let (mut tx, mut rxs) = broadcast::channel::<u64, 2>(64);
//! let [mut rx0, mut rx1] = rxs;
//!
//! tx.try_send(7)?;
//! let _a = *rx0.try_recv()?;
//! let _b = *rx1.try_recv()?;
//! # Ok(()) }
//! ```

use std::sync::Arc;

use crate::RingBuffer;
use crate::sequencers::{
    FanoutConSeqGate, FanoutConsumerSequencer, MultiPubSeqGate, MultiPublisherSequencer,
};

type PublisherSequencer<const N: usize> = MultiPublisherSequencer<FanoutConSeqGate<N>>;
type ConsumerSequencer = FanoutConsumerSequencer<MultiPubSeqGate>;

type Publisher<const N: usize, T> = crate::publisher::Publisher<PublisherSequencer<N>, T>;
type Consumer<T> = crate::consumers::Consumer<ConsumerSequencer, T>;

/// Creates a bounded broadcast channel.
///
/// `capacity` must be a power of two.
///
/// `N` is the number of receivers (fixed at creation).
///
/// The ring buffer is pre-allocated and initialized with `T::default()`.
pub fn channel<T, const N: usize>(capacity: usize) -> (Sender<T, N>, [Receiver<T>; N])
where
    T: Default,
{
    let ring_buffer = Arc::new(RingBuffer::init_with_default(capacity));
    channel_with_ring::<T, N>(ring_buffer)
}

/// Creates a bounded broadcast channel, initializing the ring buffer with `initializer`.
///
/// `capacity` must be a power of two.
/// `N` is the number of receivers (fixed at creation).
/// `initializer` is invoked once per slot index during pre-allocation.
/// The bound `Copy + FnOnce(usize) -> T` allows passing non-capturing closures and function pointers while still calling the initializer multiple times.
pub fn channel_with<T, I, const N: usize>(
    capacity: usize,
    initializer: I,
) -> (Sender<T, N>, [Receiver<T>; N])
where
    I: Copy + FnOnce(usize) -> T,
{
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    channel_with_ring::<T, N>(ring_buffer)
}

fn channel_with_ring<T, const N: usize>(
    ring_buffer: Arc<RingBuffer<T>>,
) -> (Sender<T, N>, [Receiver<T>; N]) {
    let capacity = ring_buffer.capacity() as usize;
    let ring_meta = crate::RingBufferMeta::new(capacity);

    let consumed: [Arc<crate::Cursor>; N] =
        std::array::from_fn(|_| Arc::new(crate::Cursor::new(crate::Sequence::INIT)));
    let consumer_gate = Arc::new(FanoutConSeqGate::<N>::new(consumed.clone()));
    let disconnect_counter = consumer_gate.disconnect_counter();

    let publisher_sequencer = PublisherSequencer::<N>::new(consumer_gate, ring_meta);
    let publisher_gate = Arc::new(publisher_sequencer.publisher_gate());

    let sender = Sender::<T, N> {
        inner: Publisher::<N, T>::new(publisher_sequencer, ring_buffer.clone()),
    };

    let receivers: [Receiver<T>; N] = std::array::from_fn(|i| {
        let consumer_sequencer = ConsumerSequencer::new(
            publisher_gate.clone(),
            consumed[i].clone(),
            disconnect_counter.clone(),
            ring_meta,
        );
        Receiver {
            inner: Consumer::new(consumer_sequencer, ring_buffer.clone()),
        }
    });

    (sender, receivers)
}

channel_define_sender! {
    /// The sending half of a broadcast channel.
    ///
    /// This type is `Clone`.
    #[derive(Clone)]
    pub struct Sender<T, const N: usize> {
        inner: Publisher<N, T>,
    }
    => SendBatch = SendBatch;
}

channel_define_send_batch! {
    pub struct SendBatch<'a, T, F, const N: usize> = (PublisherSequencer<N>);
}

channel_define_receiver! {
    /// The receiving half of a broadcast channel.
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
