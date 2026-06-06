//! Multi-Producer, multi-consumer fan-out channel.
//!
//! This is the fixed-N, LMAX/Disruptor-style topology where each receiver
//! observes every published item.
//! Senders are gated by the slowest receiver.
//!
//! Dropping a receiver disconnects and removes it from the gating set.
//! Once all receivers are dropped, send operations observe `Disconnected`.
//!
//! A dropped receiver will not be able to reconnect to the channel.
//!
//! # Capacity
//!
//! `capacity` must be a power of two.
//!
//! # Examples
//!
//! ```rust
#![doc = include_str!("../examples/fanout_usage.rs")]
//! ```

use std::sync::Arc;

use crate::errors::{InvalidChannelSize, TryReserveError, TrySendAtMostError, TrySendError};
use crate::receiver::{ReadRefImpl, ReadRefsImpl};
use crate::sequencers::{
    FanoutConSeqGate, FanoutConsumerSequencer, MultiPubSeqGate, MultiPublisherSequencer,
};
use crate::{
    ChanReadRef, ChanReadRefs, ChanReceiver, ChanWritePermit, ChanWritePermits, ChannelSender,
    RingBuffer, Sentinel,
};

type PublisherSequencer<const N: usize> = MultiPublisherSequencer<FanoutConSeqGate<N>>;
type ConsumerSequencer = FanoutConsumerSequencer<MultiPubSeqGate>;

type Publisher<const N: usize, T> = crate::sender::SenderImpl<PublisherSequencer<N>, T>;
type Consumer<T> = crate::receiver::ReceiverImpl<ConsumerSequencer, T>;

/// Creates a bounded broadcast channel.
///
/// `capacity` must be a power of two.
///
/// `N` is the number of receivers (fixed at creation).
///
/// The ring buffer is pre-allocated and initialized with `T::default()`.
pub fn channel<T, const N: usize>(
    capacity: usize,
) -> Result<(Sender<N, T>, [Receiver<T>; N]), InvalidChannelSize>
where
    T: Sentinel,
{
    InvalidChannelSize::validate(capacity)?;
    let ring_buffer = Arc::new(RingBuffer::init_with_sentinel(capacity));
    Ok(channel_with_ring::<T, N>(ring_buffer))
}

/// Creates a bounded broadcast channel, initializing the ring buffer with `initializer`.
///
/// `capacity` must be a power of two.
/// `N` is the number of receivers (fixed at creation).
/// `initializer` is invoked once per slot during pre-allocation.
/// The bound `Copy + FnOnce() -> T` allows passing non-capturing closures and
/// function pointers while still calling the initializer multiple times.
pub fn channel_with<T, I, const N: usize>(
    capacity: usize,
    initializer: I,
) -> Result<(Sender<N, T>, [Receiver<T>; N]), InvalidChannelSize>
where
    I: Copy + FnOnce() -> T,
    T: Sentinel,
{
    InvalidChannelSize::validate(capacity)?;
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    Ok(channel_with_ring::<T, N>(ring_buffer))
}

fn channel_with_ring<T, const N: usize>(
    ring_buffer: Arc<RingBuffer<T>>,
) -> (Sender<N, T>, [Receiver<T>; N])
where
    T: Sentinel,
{
    let ring_meta = ring_buffer.meta();
    let consumed: [Arc<crate::Cursor>; N] =
        std::array::from_fn(|_| Arc::new(crate::Cursor::new(crate::Sequence::INIT)));
    let consumer_gate = Arc::new(FanoutConSeqGate::<N>::new(consumed.clone()));

    let publisher_sequencer = PublisherSequencer::<N>::new(consumer_gate, ring_meta);
    let publisher_gate = Arc::new(publisher_sequencer.publisher_gate());

    let sender = Sender::<N, T> {
        inner: Publisher::<N, T>::new(publisher_sequencer, ring_buffer.clone()),
    };

    let receivers: [Receiver<T>; N] = std::array::from_fn(|i| {
        let consumer_sequencer =
            ConsumerSequencer::new(publisher_gate.clone(), consumed[i].clone(), ring_meta);
        Receiver {
            inner: Consumer::new(consumer_sequencer, ring_buffer.clone()),
        }
    });

    (sender, receivers)
}

/// A fan-out channel sender that can send items to multiple receivers.
#[derive(Clone)]
pub struct Sender<const N: usize, T> {
    inner: Publisher<N, T>,
}

impl<const N: usize, T> ChannelSender<T> for Sender<N, T>
where
    T: Sentinel,
{
    type WritePermit<'this>
        = WritePermit<'this, N, T>
    where
        Self: 'this;

    type WritePermits<'this>
        = WritePermits<'this, N, T>
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

/// A fan-out channel permit for writing a single item to the channel.
pub struct WritePermit<'a, const N: usize, T: Sentinel> {
    inner: crate::guards::WritePermitImpl<'a, T, PublisherSequencer<N>>,
}

/// A batch of reserved slots for writing multiple consecutive items to a fan-out channel.
pub struct WritePermits<'a, const N: usize, T: Sentinel> {
    inner: crate::guards::WritePermitsImpl<'a, T, PublisherSequencer<N>>,
}

impl<const N: usize, T: Sentinel> ChanWritePermit<T> for WritePermit<'_, N, T> {
    fn write(self, item: T) {
        self.inner.write(item);
    }
}

impl<'a, const N: usize, T: Sentinel> ChanWritePermits<T> for WritePermits<'a, N, T> {
    fn next(&mut self) -> Option<impl ChanWritePermit<T>> {
        self.inner.next()
    }

    fn commit(self) {
        self.inner.commit();
    }

    fn total_reserved(&self) -> usize {
        self.inner.total_reserved()
    }
}

/// A receiver for a fan-out channel.
pub struct Receiver<T> {
    inner: Consumer<T>,
}

/// A read reference for a fan-out channel.
pub struct ReadRef<'a, T> {
    inner: ReadRefImpl<'a, T, ConsumerSequencer>,
}

/// A read reference for a batch of values for a fan-out channel.
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
