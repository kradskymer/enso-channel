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
    DefaultConsumerSequencer, FanoutConSeqGate, MultiPubSeqGate, MultiPublisherSequencer,
};
use crate::{
    ChanReadRef, ChanReadRefs, ChanReceiver, ChanSender, ChanWritePermit, ChanWritePermits,
    RingBuffer, SlotRecycler,
};

type PublisherSequencer<const N: usize> = MultiPublisherSequencer<FanoutConSeqGate<N>>;
type ConsumerSequencer = DefaultConsumerSequencer<MultiPubSeqGate>;

type Publisher<const N: usize, T> = crate::sender::SenderImpl<PublisherSequencer<N>, T>;
type Consumer<T> = crate::receiver::ReceiverImpl<ConsumerSequencer, T>;

/// Creates a bounded broadcast channel.
///
/// `capacity` must be a power of two.
///
/// `N` is the number of receivers (fixed at creation).
///
/// The ring buffer is pre-allocated and initialized with `T::default()`.
pub fn channel<const N: usize, T: Default>(
    capacity: usize,
) -> Result<(Sender<N, T>, [Receiver<T>; N]), InvalidChannelSize> {
    InvalidChannelSize::validate(capacity)?;
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, T::default));
    Ok(channel_with_ring::<T, N>(ring_buffer))
}

/// Creates a bounded broadcast channel, initializing the ring buffer with `initializer`.
///
/// `capacity` must be a power of two.
/// `N` is the number of receivers (fixed at creation).
/// `initializer` is invoked once per slot during pre-allocation.
/// The bound `Copy + FnOnce() -> T` allows passing non-capturing closures and
/// function pointers while still calling the initializer multiple times.
pub fn channel_with<const N: usize, T, I>(
    capacity: usize,
    initializer: I,
) -> Result<(Sender<N, T>, [Receiver<T>; N]), InvalidChannelSize>
where
    I: Fn() -> T,
{
    InvalidChannelSize::validate(capacity)?;
    let ring_buffer = Arc::new(RingBuffer::init_with(capacity, initializer));
    Ok(channel_with_ring::<T, N>(ring_buffer))
}

fn channel_with_ring<T, const N: usize>(
    ring_buffer: Arc<RingBuffer<T>>,
) -> (Sender<N, T>, [Receiver<T>; N]) {
    let ring_meta = ring_buffer.meta();
    let consumer_gate = Arc::new(FanoutConSeqGate::<N>::new());

    let publisher_sequencer = PublisherSequencer::<N>::new(consumer_gate.clone(), ring_meta);
    let publisher_gate = Arc::new(publisher_sequencer.publisher_gate());

    let sender = Sender::<N, T> {
        inner: Publisher::<N, T>::new(publisher_sequencer, ring_buffer.clone()),
    };

    let receivers: [Receiver<T>; N] = cfg_select! {
        feature = "async-receiver" => {
            std::array::from_fn(|i| {
                let consumer_sequencer = ConsumerSequencer::new(
                    publisher_gate.clone(),
                    consumer_gate.consumed[i].clone(),
                    ring_meta,
                    consumer_gate.wakers[i].clone(),
                );
                Receiver {
                    inner: Consumer::new(consumer_sequencer, ring_buffer.clone()),
                }
            })
        }
        _ => {
            std::array::from_fn(|i| {
                let consumer_sequencer = ConsumerSequencer::new(
                    publisher_gate.clone(),
                    consumer_gate.consumed[i].clone(),
                    ring_meta,
                );
                Receiver {
                    inner: Consumer::new(consumer_sequencer, ring_buffer.clone()),
                }
            })

        }
    };
    (sender, receivers)
}

/// A fan-out channel sender that can send items to multiple receivers.
#[derive(Clone)]
pub struct Sender<const N: usize, T> {
    inner: Publisher<N, T>,
}

impl<const N: usize, T> ChanSender<T> for Sender<N, T> {
    type WritePermit<'this, S>
        = WritePermit<'this, N, T, S>
    where
        Self: 'this,
        S: SlotRecycler<T>;

    type WritePermits<'this, S>
        = WritePermits<'this, N, T, S>
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

/// A fan-out channel permit for writing a single item to the channel.
pub struct WritePermit<'a, const N: usize, T, S: SlotRecycler<T>> {
    inner: crate::guards::WritePermitImpl<'a, T, S, PublisherSequencer<N>>,
}

/// A batch of reserved slots for writing multiple consecutive items to a fan-out channel.
pub struct WritePermits<'a, const N: usize, T, S: SlotRecycler<T>> {
    inner: crate::guards::WritePermitsImpl<'a, T, S, PublisherSequencer<N>>,
}

impl<const N: usize, T, S: SlotRecycler<T>> ChanWritePermit<T> for WritePermit<'_, N, T, S> {
    fn write(self, item: T) {
        self.inner.write(item);
    }

    fn update_in_place(self, f: impl FnOnce(&mut T)) {
        self.inner.update_in_place(f);
    }
}

impl<'a, const N: usize, T, S: SlotRecycler<T>> ChanWritePermits<T> for WritePermits<'a, N, T, S> {
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
