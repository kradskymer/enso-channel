use std::sync::{atomic::Ordering, Arc};

use crate::{
    errors::TryClaimError, sequencers::Sequencer, slot_states::SlotState, ConsumerSeqGate, Cursor,
    PublisherSeqGate, RingBufferMeta, Sequence,
};

// Removed ConsumerOps adapter

pub(crate) struct FanoutConsumerSequencer<P: PublisherSeqGate> {
    consumed: Arc<Cursor>,
    max_published: Sequence,
    producer_gate: Arc<P>,
    ring_meta: RingBufferMeta,
}

impl<P: PublisherSeqGate> FanoutConsumerSequencer<P> {
    pub fn new(producer_gate: Arc<P>, consumed: Arc<Cursor>, ring_meta: RingBufferMeta) -> Self {
        Self {
            consumed,
            max_published: Sequence::INIT,
            producer_gate,
            ring_meta,
        }
    }
}

impl<P: PublisherSeqGate> Sequencer for FanoutConsumerSequencer<P> {
    fn try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let last_consumed = Sequence::new(self.consumed.load(Ordering::Relaxed));
        let highest_to_claim = last_consumed + n;
        let next_claim = last_consumed + 1;

        if highest_to_claim <= self.max_published {
            return Ok((next_claim, highest_to_claim));
        }

        let start_seq = last_consumed + 1;
        let state = self
            .producer_gate
            .max_published(start_seq, highest_to_claim);
        let max_published = state.sequence();
        if max_published > self.max_published {
            self.max_published = max_published;
        }
        if state.is_shutdown() {
            // Producer has shutdown. Check if we've consumed all available data.
            if last_consumed >= max_published {
                return Err(TryClaimError::Shutdown);
            }
        }

        if max_published >= highest_to_claim {
            return Ok((next_claim, highest_to_claim));
        }

        Err(TryClaimError::Insufficient {
            missing: highest_to_claim.value() - max_published.value(),
        })
    }

    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        Sequencer::try_claim_n(self, 1).map(|(_, end_seq)| end_seq)
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
        self.consumed.store(seq.value(), Ordering::Release);
    }

    #[inline]
    fn commit_range(&self, _start_seq: Sequence, end_seq: Sequence) {
        self.consumed.store(end_seq.value(), Ordering::Release);
    }
}

impl<P: PublisherSeqGate> crate::sequencers::sealed::Sealed for FanoutConsumerSequencer<P> {}

impl<P: PublisherSeqGate> Drop for FanoutConsumerSequencer<P> {
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

impl<const N: usize> ConsumerSeqGate for FanoutConSeqGate<N> {
    fn max_consumed(&self, _next_seq: Sequence, _end_seq: Sequence) -> SlotState {
        let mut min_consumed = Sequence::SHUTDOWN_OPEN;
        for i in 0..N {
            let value = self.consumed[i].load(Ordering::Acquire);
            let seq = Sequence::new(value);
            min_consumed = min_consumed.min(seq);
        }
        SlotState::new(min_consumed, min_consumed.is_shutdown_open())
    }
}
