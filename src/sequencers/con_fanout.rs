use std::sync::{atomic::Ordering, Arc};

use crate::{
    errors::TryClaimError,
    sequencers::{ConsumerBarrier, ProducerBarrier, Sequencer, SlotState},
    Cursor, RingBufferMeta, Sequence,
};

pub(crate) struct FanoutConsumerSequencer<P: ProducerBarrier> {
    consumed: Arc<Cursor>,
    producer_gate: Arc<P>,
    ring_meta: RingBufferMeta,
}

impl<B: ProducerBarrier> FanoutConsumerSequencer<B> {
    pub fn new(producer_gate: Arc<B>, consumed: Arc<Cursor>, ring_meta: RingBufferMeta) -> Self {
        Self {
            consumed,
            producer_gate,
            ring_meta,
        }
    }
}

impl<B: ProducerBarrier> Sequencer for FanoutConsumerSequencer<B> {
    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.try_claim_at_most(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let last_claimed = Sequence::new(self.consumed.load(Ordering::Relaxed));
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

impl<B: ProducerBarrier> Drop for FanoutConsumerSequencer<B> {
    fn drop(&mut self) {
        // Mark this consumer as inactive so it no longer gates publishers.
        self.consumed
            .store(Sequence::SHUTDOWN_OPEN.value(), Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct FanoutConSeqGate<const N: usize> {
    consumed: [Arc<Cursor>; N],
}

impl<const N: usize> FanoutConSeqGate<N> {
    pub(crate) fn new(consumed: [Arc<Cursor>; N]) -> Self {
        Self { consumed }
    }
}

impl<const N: usize> ConsumerBarrier for FanoutConSeqGate<N> {
    fn max_consumed(&self) -> SlotState {
        let mut min_consumed = Sequence::SHUTDOWN_OPEN;
        for i in 0..N {
            let value = self.consumed[i].load(Ordering::Acquire);
            let seq = Sequence::new(value);
            min_consumed = min_consumed.min(seq);
        }
        SlotState::new(min_consumed, min_consumed.is_shutdown_open())
    }
}
