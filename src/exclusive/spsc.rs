//! Single-publisher, single-consumer (SPSC) channel.
//!
//! This module provides a bounded SPSC channel backed by a ring buffer.
//!
//! Key properties:
//! - **Bounded**: capacity is fixed at creation time.
//! - **Non-blocking**: operations are `try_*` and return an error on full/empty.
//! - **Zero-copy receive**: receives yield `&T` via guard/iterator types that
//!   commit consumption on drop.
//!
//! # Capacity
//!
//! `capacity` must be a power of two.

use std::sync::Arc;

use crate::RingBuffer;
use crate::errors::{TryRecvAtMostError, TryRecvError, TrySendAtMostError, TrySendError};
use crate::sequencers::{ExclusiveConSeqGate, ExclusiveConsumerSequencer};
use crate::sequencers::{ExclusivePubSeqGate, ExclusivePublisherSequencer};

type PublisherSequencer = ExclusivePublisherSequencer<ExclusiveConSeqGate>;

type ConsumerSequencer = ExclusiveConsumerSequencer<ExclusivePubSeqGate>;

type Publisher<T> = crate::publisher::Publisher<PublisherSequencer, T>;

type Consumer<T> = crate::consumers::Consumer<ConsumerSequencer, T>;

/// Creates a bounded SPSC channel using `T::default()` to initialize the ring.
///
/// `capacity` must be a power of two.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>)
where
    T: Default,
{
    let ring_buffer = Arc::new(RingBuffer::init_with_default(capacity));
    channel_with_ring(ring_buffer)
}

/// Creates a bounded SPSC channel, initializing the ring buffer with `initializer`.
///
/// `capacity` must be a power of two.
pub fn channel_with<T, I>(capacity: usize, initializer: I) -> (Sender<T>, Receiver<T>)
where
    I: Copy + FnOnce(usize) -> T,
{
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    channel_with_ring(ring_buffer)
}

fn channel_with_ring<T>(ring_buffer: Arc<RingBuffer<T>>) -> (Sender<T>, Receiver<T>) {
    let capacity = ring_buffer.capacity() as usize;

    // Create the consumer gate from a shared cursor so the publisher can observe consumption.
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

/// The sending half of an SPSC channel.
///
/// This type is not `Clone`.
pub struct Sender<T> {
    inner: Publisher<T>,
}

impl<T> Sender<T> {
    /// Attempts to send a single item.
    pub fn try_send(&mut self, item: T) -> Result<(), TrySendError> {
        self.inner.try_publish(item)
    }

    /// Attempts to claim a contiguous range of `n` slots and returns a guard that
    /// writes/commits the range.
    ///
    /// The returned batch commits automatically on drop.
    pub fn try_send_many<F>(
        &mut self,
        n: usize,
        factory: F,
    ) -> Result<SendBatch<'_, T, F>, TrySendError>
    where
        F: Fn() -> T + Copy,
    {
        let inner = self.inner.try_publish_many(n, factory)?;
        Ok(SendBatch { inner })
    }

    /// Attempts to claim a contiguous range of `n` slots using `T::default()` as the fill factory.
    pub fn try_send_many_default(
        &mut self,
        n: usize,
    ) -> Result<SendBatch<'_, T, fn() -> T>, TrySendError>
    where
        T: Default,
    {
        let inner = self.inner.try_publish_many_default(n)?;
        Ok(SendBatch { inner })
    }

    /// Attempts to send up to `limit` items, claiming as many slots as available.
    ///
    /// Returns a batch with the actually claimed slots (1..=limit).
    /// Returns `Full` if zero slots are available.
    pub fn try_send_at_most<F>(
        &mut self,
        limit: usize,
        factory: F,
    ) -> Result<SendBatch<'_, T, F>, TrySendAtMostError>
    where
        F: Fn() -> T + Copy,
    {
        let inner = self.inner.try_publish_at_most(limit, factory)?;
        Ok(SendBatch { inner })
    }

    /// Attempts to send up to `limit` items using `T::default()` as the fill factory.
    pub fn try_send_at_most_default(
        &mut self,
        limit: usize,
    ) -> Result<SendBatch<'_, T, fn() -> T>, TrySendAtMostError>
    where
        T: Default,
    {
        let inner = self.inner.try_publish_at_most_default(limit)?;
        Ok(SendBatch { inner })
    }
}

channel_define_send_batch! {
    /// A guard representing an already-claimed contiguous range of slots.
    ///
    /// This is returned by [`Sender::try_send_many`].
    pub struct SendBatch<'a, T, F> = (PublisherSequencer);
}

/// The receiving half of an SPSC channel.
pub struct Receiver<T> {
    inner: Consumer<T>,
}

impl<T> Receiver<T> {
    /// Attempts to receive a single item.
    ///
    /// The returned guard commits the consumed sequence on drop.
    pub fn try_recv(&mut self) -> Result<RecvGuard<'_, T>, TryRecvError> {
        let inner = self.inner.try_recv()?;
        Ok(RecvGuard { inner })
    }

    /// Attempts to receive up to `n` items.
    ///
    /// The returned iterator commits the consumed range on drop.
    pub fn try_recv_many(&mut self, n: usize) -> Result<RecvIter<'_, T>, TryRecvError> {
        let inner = self.inner.try_recv_many(n as i64)?;
        Ok(RecvIter { inner })
    }

    /// Attempts to receive up to `limit` items, consuming as many as available.
    ///
    /// Returns an iterator with the actually consumed items (1..=limit).
    /// Returns `Empty` if zero items are available.
    pub fn try_recv_at_most(
        &mut self,
        limit: usize,
    ) -> Result<RecvIter<'_, T>, TryRecvAtMostError> {
        let inner = self.inner.try_recv_at_most(limit as i64)?;
        Ok(RecvIter { inner })
    }
}

channel_define_recv_guard! {
    /// RAII guard for a received item.
    ///
    /// Dereferences to `&T`.
    pub struct RecvGuard<'a, T> = (ConsumerSequencer);
}

channel_define_recv_iter! {
    /// Iterator over a received range.
    ///
    /// Yields `&T` and commits the full range on drop.
    pub struct RecvIter<'a, T> = (ConsumerSequencer);
}
