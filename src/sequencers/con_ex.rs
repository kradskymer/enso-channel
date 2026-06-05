use std::sync::{atomic::Ordering, Arc};

use crate::sequencers::{Sequencer, SlotState};
use crate::RingBufferMeta;
use crate::{errors::TryClaimError, ConsumerSeqGate, Cursor, PublisherSeqGate, Sequence};

pub(crate) struct ExclusiveConsumerSequencer<P: PublisherSeqGate> {
    consumed: Arc<Cursor>,
    max_published: Sequence,
    producer_gate: Arc<P>,
    ring_meta: RingBufferMeta,
}

impl<P: PublisherSeqGate> ExclusiveConsumerSequencer<P> {
    pub(crate) fn new(
        consumed: Arc<Cursor>,
        producer_gate: Arc<P>,
        ring_meta: RingBufferMeta,
    ) -> Self {
        Self {
            consumed,
            max_published: Sequence::INIT,
            producer_gate,
            ring_meta,
        }
    }
}

impl<P: PublisherSeqGate> ExclusiveConsumerSequencer<P> {
    #[inline]
    fn do_commit(&self, seq: Sequence) {
        self.consumed.store(seq.value(), Ordering::Release);
    }

    #[inline]
    fn do_commit_range(&self, _start: Sequence, end: Sequence) {
        self.do_commit(end);
    }
}

impl<P: PublisherSeqGate> Sequencer for ExclusiveConsumerSequencer<P> {
    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.try_claim_at_most(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let last_claimed = Sequence::new(self.consumed.load(Ordering::Relaxed));
        let n = limit.min(self.ring_meta.buffer_size());
        let start_seq = last_claimed + 1;
        let highest_to_consume = last_claimed + n;

        if highest_to_consume > self.max_published {
            let state = self
                .producer_gate
                .max_published(start_seq, highest_to_consume);
            self.max_published = state.sequence();
        }

        if highest_to_consume <= self.max_published {
            return Ok((start_seq, highest_to_consume));
        }

        let state = self
            .producer_gate
            .max_published(start_seq, highest_to_consume);
        self.max_published = state.sequence();

        let end_seq = if state.is_shutdown() {
            if last_claimed >= self.max_published {
                return Err(TryClaimError::Shutdown);
            }

            let end_seq = self.max_published;
            if start_seq > end_seq {
                return Err(TryClaimError::Empty);
            }
            end_seq
        } else {
            if start_seq > self.max_published {
                return Err(TryClaimError::Empty);
            }
            let available = (self.max_published - last_claimed).value().min(limit);
            last_claimed + available
        };

        Ok((start_seq, end_seq))
    }

    #[inline]
    fn commit(&self, seq: Sequence) {
        self.do_commit(seq);
    }

    #[inline]
    fn commit_range(&self, start_seq: Sequence, end_seq: Sequence) {
        self.do_commit_range(start_seq, end_seq);
    }
}

impl<P: PublisherSeqGate> crate::sequencers::sealed::Sealed for ExclusiveConsumerSequencer<P> {}

impl<P: PublisherSeqGate> Drop for ExclusiveConsumerSequencer<P> {
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

impl ConsumerSeqGate for ExclusiveConSeqGate {
    fn max_consumed(&self, _next_seq: crate::Sequence, _end_seq: crate::Sequence) -> SlotState {
        let value = self.consumed.load(Ordering::Acquire);
        let seq = Sequence::new(value);
        SlotState::new(seq, seq.is_shutdown_open())
    }
}
