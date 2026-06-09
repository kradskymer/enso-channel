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
use crate::sequencers::{DefaultConsumerSequencer, ExclusiveConSeqGate};
use crate::sequencers::{MultiPubSeqGate, MultiPublisherSequencer};
use crate::{
    ChanReadRef, ChanReadRefs, ChanReceiver, ChanSender, ChanWritePermit, ChanWritePermits,
    RingBuffer, SlotRecycler,
};

type PublisherSequencer = MultiPublisherSequencer<ExclusiveConSeqGate>;
type ConsumerSequencer = DefaultConsumerSequencer<MultiPubSeqGate>;

type Publisher<T> = crate::sender::SenderImpl<PublisherSequencer, T>;
type Consumer<T> = crate::receiver::ReceiverImpl<ConsumerSequencer, T>;

/// Creates a bounded MPSC channel using `T::default()` to initialize the ring.
///
/// `capacity` must be a power of two.
pub fn channel<T: Default>(
    capacity: usize,
) -> Result<(Sender<T>, Receiver<T>), InvalidChannelSize> {
    InvalidChannelSize::validate(capacity)?;
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, T::default));
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
    I: Fn() -> T,
{
    InvalidChannelSize::validate(capacity)?;
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    Ok(channel_with_ring(ring_buffer))
}

fn channel_with_ring<T>(ring_buffer: Arc<RingBuffer<T>>) -> (Sender<T>, Receiver<T>) {
    let capacity = ring_buffer.capacity() as usize;

    let consumer_gate = Arc::new(ExclusiveConSeqGate::new());

    let ring_meta = crate::RingBufferMeta::new(capacity);
    let publisher_sequencer = PublisherSequencer::new(consumer_gate.clone(), ring_meta);
    let publisher_gate = Arc::new(publisher_sequencer.publisher_gate());

    let consumer_sequencer = cfg_select! {
        feature = "async-receiver" => {
            ConsumerSequencer::new(
                publisher_gate,
                consumer_gate.consumed.clone(),
                ring_meta,
                consumer_gate.waker.clone(),
            )
        },
        _ => {
            ConsumerSequencer::new(
                publisher_gate,
                consumer_gate.consumed.clone(),
                ring_meta,
            )
        }
    };

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

impl<T> ChanSender<T> for Sender<T> {
    type WritePermit<'this, S>
        = WritePermit<'this, T, S>
    where
        Self: 'this,
        S: SlotRecycler<T>;

    type WritePermits<'this, S>
        = WritePermits<'this, T, S>
    where
        Self: 'this,
        S: SlotRecycler<T>;

    fn try_send(&mut self, value: T) -> Result<(), TrySendError<T>> {
        self.inner.try_send(value)
    }

    fn try_reserve<S: SlotRecycler<T>>(
        &mut self,
        recycler: S,
    ) -> Result<Self::WritePermit<'_, S>, TryReserveError> {
        let permit = self.inner.try_reserve(recycler)?;
        Ok(WritePermit { inner: permit })
    }

    fn try_send_at_most<S: SlotRecycler<T>>(
        &mut self,
        limit: usize,
        recycler: S,
    ) -> Result<Self::WritePermits<'_, S>, TrySendAtMostError> {
        let batch_permits = self.inner.try_send_at_most(limit, recycler)?;
        Ok(WritePermits {
            inner: batch_permits,
        })
    }
}

/// Permit for writing a single item to an MPSC channel.
pub struct WritePermit<'a, T, S: SlotRecycler<T>> {
    inner: crate::guards::WritePermitImpl<'a, T, S, PublisherSequencer>,
}

/// Permit for writing a batch of items to an MPSC channel.
pub struct WritePermits<'a, T, S: SlotRecycler<T>> {
    inner: crate::guards::WritePermitsImpl<'a, T, S, PublisherSequencer>,
}

impl<T, S: SlotRecycler<T>> ChanWritePermit<T> for WritePermit<'_, T, S> {
    fn write(self, item: T) {
        self.inner.write(item);
    }

    fn update_in_place(self, f: impl FnOnce(&mut T)) {
        self.inner.update_in_place(f);
    }
}

impl<'a, T, S: SlotRecycler<T>> ChanWritePermits<T> for WritePermits<'a, T, S> {
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
    fn iter(&self) -> impl Iterator<Item = &'a T> + '_ {
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

    #[cfg(feature = "async-receiver")]
    async fn recv_async(&mut self) -> Option<Self::ReadRef<'_>> {
        self.inner.recv_async().await.map(|inner| ReadRef { inner })
    }

    #[cfg(feature = "async-receiver")]
    async fn recv_at_most_async<'a>(&'a mut self, limit: usize) -> Option<Self::ReadRefs<'a>>
    where
        T: 'a,
    {
        self.inner
            .recv_at_most_async(limit)
            .await
            .map(|inner| ReadRefs { inner })
    }
}
