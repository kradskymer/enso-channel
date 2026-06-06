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
#![doc = include_str!("../examples/mpsc_usage.rs")]
//! ```

use std::sync::Arc;

use crate::errors::{InvalidChannelSize, TryReserveError, TrySendAtMostError, TrySendError};
use crate::receiver::{ReadRefImpl, ReadRefsImpl};
use crate::sequencers::{ExclusiveConSeqGate, ExclusiveConsumerSequencer};
use crate::sequencers::{MultiPubSeqGate, MultiPublisherSequencer};
use crate::{
    ChanReadRef, ChanReadRefs, ChanReceiver, ChanWritePermit, ChanWritePermits, ChannelSender,
    RingBuffer, Sentinel,
};

type PublisherSequencer = MultiPublisherSequencer<ExclusiveConSeqGate>;
type ConsumerSequencer = ExclusiveConsumerSequencer<MultiPubSeqGate>;

type Publisher<T> = crate::sender::SenderImpl<PublisherSequencer, T>;
type Consumer<T> = crate::receiver::ReceiverImpl<ConsumerSequencer, T>;

/// Creates a bounded MPSC channel using `T::default()` to initialize the ring.
///
/// `capacity` must be a power of two.
pub fn channel<T>(capacity: usize) -> Result<(Sender<T>, Receiver<T>), InvalidChannelSize>
where
    T: Sentinel,
{
    InvalidChannelSize::validate(capacity)?;
    let ring_buffer = Arc::new(RingBuffer::init_with_sentinel(capacity));
    Ok(channel_with_ring(ring_buffer))
}

/// Creates a bounded MPSC channel, initializing the ring buffer with `initializer`.
///
/// `capacity` must be a power of two.
///
/// `initializer` is invoked once per slot during pre-allocation.
/// The bound `Copy + FnOnce() -> T` allows passing non-capturing closures and
/// function pointers while still calling the initializer multiple times.
pub fn channel_with<T, I>(
    capacity: usize,
    initializer: I,
) -> Result<(Sender<T>, Receiver<T>), InvalidChannelSize>
where
    I: Copy + FnOnce() -> T,
    T: Sentinel,
{
    InvalidChannelSize::validate(capacity)?;
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    Ok(channel_with_ring(ring_buffer))
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
        = WritePermits<'this, T>
    where
        Self: 'this;

    fn try_send(&mut self, value: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(value)
    }

    fn try_reserve(&mut self) -> Result<Self::WritePermit<'_>, TryReserveError> {
        let permit = self.inner.try_reserve()?;
        Ok(WritePermit { inner: permit })
    }

    fn try_send_at_most(
        &mut self,
        limit: usize,
    ) -> Result<Self::WritePermits<'_>, TrySendAtMostError> {
        let batch_permits = self.inner.try_send_at_most(limit)?;
        Ok(WritePermits {
            inner: batch_permits,
        })
    }
}

/// Permit for writing a single item to an MPSC channel.
pub struct WritePermit<'a, T: Sentinel> {
    inner: crate::guards::WritePermitImpl<'a, T, PublisherSequencer>,
}

/// Permit for writing a batch of items to an MPSC channel.
pub struct WritePermits<'a, T: Sentinel> {
    inner: crate::guards::WritePermitsImpl<'a, T, PublisherSequencer>,
}

impl<T: Sentinel> ChanWritePermit<T> for WritePermit<'_, T> {
    fn write(self, item: T) {
        self.inner.write(item);
    }
}

impl<'a, T: Sentinel> ChanWritePermits<T> for WritePermits<'a, T> {
    fn total_reserved(&self) -> usize {
        self.inner.total_reserved()
    }

    fn next(&mut self) -> Option<impl ChanWritePermit<T>> {
        self.inner.next()
    }

    fn commit(self) {
        self.inner.commit();
    }
}

/// A receiver for the MPSC channel.
pub struct Receiver<T> {
    inner: Consumer<T>,
}

/// A read reference for a single value from the MPSC channel.
pub struct ReadRef<'a, T> {
    inner: ReadRefImpl<'a, T, ConsumerSequencer>,
}

/// A read reference for a batch of values from the MPSC channel.
pub struct ReadRefs<'a, T> {
    inner: ReadRefsImpl<'a, T, ConsumerSequencer>,
}

impl<'a, T> std::ops::Deref for ReadRef<'a, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.inner.deref()
    }
}

impl<'a, T> ChanReadRef<'a, T> for ReadRef<'a, T> {
    fn finish(self) {
        self.inner.finish();
    }
}

impl<'a, T> ChanReadRefs<'a, T> for ReadRefs<'a, T> {
    fn iter(&'a self) -> impl Iterator<Item = &'a T> + 'a {
        self.inner.iter()
    }

    fn finish(self) {
        self.inner.finish();
    }
}

impl<T> ChanReceiver<T> for Receiver<T> {
    type ReadRef<'this>
        = ReadRef<'this, T>
    where
        Self: 'this;

    type ReadRefs<'this>
        = ReadRefs<'this, T>
    where
        Self: 'this,
        T: 'this;

    fn try_recv(&mut self) -> Result<Self::ReadRef<'_>, crate::errors::TryRecvError> {
        self.inner.try_recv().map(|inner| ReadRef { inner })
    }

    fn try_recv_at_most(
        &mut self,
        limit: usize,
    ) -> Result<Self::ReadRefs<'_>, crate::errors::TryRecvError> {
        self.inner
            .try_recv_at_most(limit)
            .map(|inner| ReadRefs { inner })
    }
}
