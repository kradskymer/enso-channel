use std::sync::{atomic::Ordering, Arc};

use crate::{
    errors::TryClaimError,
    sequencers::{ConsumerSequencer, Sequencer},
    Cursor, ProducerBarrier, RingBufferMeta, Sequence,
};

pub(crate) struct DefaultConsumerSequencer<P: ProducerBarrier> {
    consumed: Arc<Cursor>,
    producer_gate: Arc<P>,
    ring_meta: RingBufferMeta,
    #[cfg(feature = "async-receiver")]
    waker: Arc<crate::waker::Waker>,
}

impl<P: ProducerBarrier> DefaultConsumerSequencer<P> {
    #[cfg(feature = "async-receiver")]
    pub fn new(
        producer_gate: Arc<P>,
        consumed: Arc<Cursor>,
        ring_meta: RingBufferMeta,
        waker: Arc<crate::waker::Waker>,
    ) -> Self {
        Self {
            consumed,
            producer_gate,
            ring_meta,
            waker,
        }
    }

    #[cfg(not(feature = "async-receiver"))]
    pub fn new(producer_gate: Arc<P>, consumed: Arc<Cursor>, ring_meta: RingBufferMeta) -> Self {
        Self {
            consumed,
            producer_gate,
            ring_meta,
        }
    }
}

impl<P: ProducerBarrier> Sequencer for DefaultConsumerSequencer<P> {
    fn try_claim(&self) -> Result<Sequence, TryClaimError> {
        self.try_claim_at_most(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let last_claimed = Sequence::new(self.consumed.load(Ordering::Acquire));
        let n = limit.min(self.ring_meta.buffer_size());
        let start_seq = last_claimed + 1;
        let highest_to_consume = last_claimed + n;
        let max_available_slot = self
            .producer_gate
            .max_published(start_seq, highest_to_consume);

        let max_available = max_available_slot.sequence();
        if max_available <= last_claimed {
            if max_available_slot.is_shutdown() {
                return Err(TryClaimError::Shutdown);
            } else {
                return Err(TryClaimError::Empty);
            }
        }

        let end_seq = max_available;

        Ok((start_seq, end_seq))
    }

    #[inline]
    fn commit(&self, seq: Sequence) {
        self.consumed.store(seq.value(), Ordering::Release);
    }

    #[inline]
    fn commit_range(&self, _start_seq: Sequence, end_seq: Sequence) {
        self.consumed.store(end_seq.value(), Ordering::Release);
    }
}

impl<P: ProducerBarrier> Drop for DefaultConsumerSequencer<P> {
    fn drop(&mut self) {
        // Mark this consumer as inactive so it no longer gates publishers.
        self.consumed
            .store(Sequence::SHUTDOWN_OPEN.value(), Ordering::Release);
    }
}

impl<P: ProducerBarrier> ConsumerSequencer for DefaultConsumerSequencer<P> {
    #[cfg(feature = "async-receiver")]
    fn claim_at_most_async(
        &self,
        limit: i64,
        ctx: &std::task::Context<'_>,
    ) -> std::task::Poll<Result<(Sequence, Sequence), TryClaimError>> {
        use std::task::Poll;

        match self.try_claim_at_most(limit) {
            Ok(result) => return Poll::Ready(Ok(result)),
            Err(TryClaimError::Shutdown) => {
                return Poll::Ready(Err(TryClaimError::Shutdown));
            }
            Err(TryClaimError::Empty) => {}
        }

        self.waker.register(ctx.waker());

        match self.try_claim_at_most(limit) {
            Ok(result) => Poll::Ready(Ok(result)),
            Err(TryClaimError::Empty) => Poll::Pending,
            Err(TryClaimError::Shutdown) => Poll::Ready(Err(TryClaimError::Shutdown)),
        }
    }
}
