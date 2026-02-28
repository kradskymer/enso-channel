//! Multi-publisher, multi-consumer broadcast channel.
//!
//! This is the fixed-N, LMAX/Disruptor-style topology where each receiver
//! observes every published item and publishers are gated by the slowest
//! receiver.

use std::sync::Arc;

use crate::RingBuffer;
use crate::errors::{TryRecvAtMostError, TryRecvError, TrySendAtMostError, TrySendError};
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
/// `N` is the number of receivers and must be small (planned max: 8).
pub fn channel<T, const N: usize>(capacity: usize) -> (Sender<T, N>, [Receiver<T>; N])
where
    T: Default,
{
    let ring_buffer = Arc::new(RingBuffer::init_with_default(capacity));
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

/// The sending half of a broadcast channel.
///
/// This type is `Clone`.
#[derive(Clone)]
pub struct Sender<T, const N: usize> {
    inner: Publisher<N, T>,
}

impl<T, const N: usize> Sender<T, N> {
    pub fn try_send(&mut self, item: T) -> Result<(), TrySendError> {
        self.inner.try_publish(item)
    }

    pub fn try_send_many<F>(
        &mut self,
        n: usize,
        factory: F,
    ) -> Result<SendBatch<'_, T, F, N>, TrySendError>
    where
        F: Fn() -> T + Copy,
    {
        let inner = self.inner.try_publish_many(n, factory)?;
        Ok(SendBatch { inner })
    }

    pub fn try_send_many_default(
        &mut self,
        n: usize,
    ) -> Result<SendBatch<'_, T, fn() -> T, N>, TrySendError>
    where
        T: Default,
    {
        let inner = self.inner.try_publish_many_default(n)?;
        Ok(SendBatch { inner })
    }

    pub fn try_send_at_most<F>(
        &mut self,
        limit: usize,
        factory: F,
    ) -> Result<SendBatch<'_, T, F, N>, TrySendAtMostError>
    where
        F: Fn() -> T + Copy,
    {
        let inner = self.inner.try_publish_at_most(limit, factory)?;
        Ok(SendBatch { inner })
    }

    pub fn try_send_at_most_default(
        &mut self,
        limit: usize,
    ) -> Result<SendBatch<'_, T, fn() -> T, N>, TrySendAtMostError>
    where
        T: Default,
    {
        let inner = self.inner.try_publish_at_most_default(limit)?;
        Ok(SendBatch { inner })
    }
}

channel_define_send_batch! {
    pub struct SendBatch<'a, T, F, const N: usize> = (PublisherSequencer<N>);
}

pub struct Receiver<T> {
    inner: Consumer<T>,
}

impl<T> Receiver<T> {
    pub fn try_recv(&mut self) -> Result<RecvGuard<'_, T>, TryRecvError> {
        let inner = self.inner.try_recv()?;
        Ok(RecvGuard { inner })
    }

    pub fn try_recv_many(&mut self, _n: usize) -> Result<RecvIter<'_, T>, TryRecvError> {
        let inner = self.inner.try_recv_many(_n as i64)?;
        Ok(RecvIter { inner })
    }

    pub fn try_recv_at_most(
        &mut self,
        limit: usize,
    ) -> Result<RecvIter<'_, T>, TryRecvAtMostError> {
        let inner = self.inner.try_recv_at_most(limit as i64)?;
        Ok(RecvIter { inner })
    }
}

channel_define_recv_guard! {
    pub struct RecvGuard<'a, T> = (ConsumerSequencer);
}

channel_define_recv_iter! {
    pub struct RecvIter<'a, T> = (ConsumerSequencer);
}
