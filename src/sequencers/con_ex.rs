use std::sync::{atomic::Ordering, Arc};

use crate::sequencers::{ConsumerBarrier, Sequencer, SlotState};
use crate::RingBufferMeta;
use crate::{errors::TryClaimError, Cursor, ProducerBarrier, Sequence};

pub(crate) struct ExclusiveConsumerSequencer<P: ProducerBarrier> {
    consumed: Arc<Cursor>,
    producer_gate: Arc<P>,
    ring_meta: RingBufferMeta,
}

impl<B: ProducerBarrier> ExclusiveConsumerSequencer<B> {
    pub(crate) fn new(
        consumed: Arc<Cursor>,
        producer_gate: Arc<B>,
        ring_meta: RingBufferMeta,
    ) -> Self {
        Self {
            consumed,
            producer_gate,
            ring_meta,
        }
    }
}

impl<P: ProducerBarrier> Sequencer for ExclusiveConsumerSequencer<P> {
    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.try_claim_at_most(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let last_claimed = Sequence::new(self.consumed.load(Ordering::Relaxed));
        let n = limit.min(self.ring_meta.buffer_size());
        let start_seq = last_claimed + 1;
        let highest_to_consume = last_claimed + n;

        let max_slot_state = self
            .producer_gate
            .max_published(start_seq, highest_to_consume);
        let end_seq = max_slot_state.sequence();
        if end_seq <= last_claimed {
            if max_slot_state.is_shutdown() {
                return Err(TryClaimError::Shutdown);
            } else {
                return Err(TryClaimError::Empty);
            }
        }

        Ok((start_seq, end_seq))
    }

    #[inline]
    fn commit(&self, seq: Sequence) {
        self.consumed.store(seq.value(), Ordering::Release);
    }

    #[inline]
    fn commit_range(&self, _start_seq: Sequence, end_seq: Sequence) {
        self.commit(end_seq);
    }
}

impl<P: ProducerBarrier> Drop for ExclusiveConsumerSequencer<P> {
    fn drop(&mut self) {
        // Encode consumer disconnect for publishers using the consumed cursor sentinel.
        // After disconnect, publishers must stop producing.
        self.consumed
            .store(Sequence::SHUTDOWN_OPEN.value(), Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct ExclusiveConSeqGate {
    consumed: Arc<Cursor>,
}

impl ExclusiveConSeqGate {
    pub(crate) fn new(consumed: Arc<Cursor>) -> Self {
        Self { consumed }
    }
}

impl ConsumerBarrier for ExclusiveConSeqGate {
    fn max_consumed(&self) -> SlotState {
        let value = self.consumed.load(Ordering::Acquire);
        let seq = Sequence::new(value);
        SlotState::new(seq, seq.is_shutdown_open())
    }
}
