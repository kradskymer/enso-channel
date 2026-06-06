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
//! ```rust
//! #![doc = include_str!("../examples/mpsc_usage.rs")]
//! ```

use std::sync::Arc;

use crate::errors::{TrySendAtMostError, TrySendError};
use crate::sequencers::{ExclusiveConSeqGate, ExclusiveConsumerSequencer};
use crate::sequencers::{MultiPubSeqGate, MultiPublisherSequencer};
use crate::{ChanWritePermit, ChanWritePermits, ChannelSender, RingBuffer, Sentinel};

type PublisherSequencer = MultiPublisherSequencer<ExclusiveConSeqGate>;
type ConsumerSequencer = ExclusiveConsumerSequencer<MultiPubSeqGate>;

type Publisher<T> = crate::sender::SenderImpl<PublisherSequencer, T>;
type Consumer<T> = crate::consumers::Consumer<ConsumerSequencer, T>;

/// Creates a bounded MPSC channel using `T::default()` to initialize the ring.
///
/// `capacity` must be a power of two.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>)
where
    T: Sentinel,
{
    let ring_buffer = Arc::new(RingBuffer::init_with_sentinel(capacity));
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
    I: Copy + FnOnce() -> T,
    T: Sentinel,
{
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    channel_with_ring(ring_buffer)
}

fn channel_with_ring<T>(ring_buffer: Arc<RingBuffer<T>>) -> (Sender<T>, Receiver<T>)
where
    T: Sentinel,
{
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

/// Sender for an MPSC channel.
#[derive(Clone)]
pub struct Sender<T> {
    inner: Publisher<T>,
}

impl<T: Sentinel> ChannelSender<T> for Sender<T> {
    type WritePermit<'this>
        = WritePermit<'this, T>
    where
        Self: 'this;

    type WritePermits<'this>
        = WritePermitsBatch<'this, T>
    where
        Self: 'this;

    fn try_send(&mut self, value: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(value)
    }

    fn try_reserve(&mut self) -> Result<Self::WritePermit<'_>, TrySendError<()>> {
        let permit = self.inner.try_reserve()?;
        Ok(WritePermit { inner: permit })
    }

    fn try_send_at_most(
        &mut self,
        limit: usize,
    ) -> Result<Self::WritePermits<'_>, TrySendAtMostError> {
        let batch_permits = self.inner.try_send_at_most(limit)?;
        Ok(WritePermitsBatch {
            inner: batch_permits,
        })
    }
}

/// Permit for writing a single item to an MPSC channel.
pub struct WritePermit<'a, T: Sentinel> {
    inner: crate::permit::WritePermitImpl<'a, T, PublisherSequencer>,
}

/// Permit for writing a batch of items to an MPSC channel.
pub struct WritePermitsBatch<'a, T: Sentinel> {
    inner: crate::permit::WritePermitsBatchImpl<'a, T, PublisherSequencer>,
}

/// Permit for writing a single item in a [`BatchPermits`] to an MPSC channel.
pub struct BatchWritePermit<'batch, 'a, T: Sentinel> {
    inner: crate::permit::BatchWritePermitImpl<'batch, 'a, T, PublisherSequencer>,
}

impl<T: Sentinel> ChanWritePermit<T> for WritePermit<'_, T> {
    fn write(self, item: T) {
        self.inner.write(item);
    }
}

impl<T: Sentinel> ChanWritePermit<T> for BatchWritePermit<'_, '_, T> {
    fn write(self, item: T) {
        self.inner.write(item);
    }
}

impl<'a, T: Sentinel> ChanWritePermits<T> for WritePermitsBatch<'a, T> {
    type Permit<'this>
        = BatchWritePermit<'this, 'a, T>
    where
        Self: 'this;

    fn batch_size(&self) -> usize {
        self.inner.batch_size()
    }

    fn next(&mut self) -> Option<BatchWritePermit<'_, 'a, T>> {
        let permit = self.inner.next()?;
        Some(BatchWritePermit { inner: permit })
    }

    fn commit(self) {
        self.inner.commit();
    }
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
