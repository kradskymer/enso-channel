//! Multi-publisher, multi-consumer (MPMC) work queue.
//!
//! Construction follows `let (tx, rx) = channel(..)`; both endpoints may be cloned.

use std::sync::Arc;

use crate::RingBuffer;
use crate::errors::{TryRecvAtMostError, TryRecvError, TrySendAtMostError, TrySendError};
use crate::sequencers::{
    MultiPubSeqGate, MultiPublisherSequencer, QueuedConSeqGate, QueuedConsumerSequencer,
    QueuedConsumerWiring,
};

type PublisherSequencer = MultiPublisherSequencer<QueuedConSeqGate>;
type ConsumerSequencer = QueuedConsumerSequencer<MultiPubSeqGate>;

type Publisher<T> = crate::publisher::Publisher<PublisherSequencer, T>;
type Consumer<T> = crate::consumers::Consumer<ConsumerSequencer, T>;

/// Creates a bounded work-stealing MPMC channel.
///
/// `capacity` must be a power of two.
pub fn channel<T>(capacity: usize) -> (Sender<T>, Receiver<T>)
where
    T: Default,
{
    let ring_buffer = Arc::new(RingBuffer::init_with_default(capacity));
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

/// The sending half of a work-stealing MPMC channel.
///
/// This type is `Clone`.
#[derive(Clone)]
pub struct Sender<T> {
    inner: Publisher<T>,
}

impl<T> Sender<T> {
    pub fn try_send(&mut self, item: T) -> Result<(), TrySendError> {
        self.inner.try_publish(item)
    }

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
    pub struct SendBatch<'a, T, F> = (PublisherSequencer);
}

/// The receiving half of a work-stealing channel.
///
/// This type is `Clone` to allow spawning multiple workers.
#[derive(Clone)]
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
