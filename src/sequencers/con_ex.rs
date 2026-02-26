use std::sync::{Arc, atomic::Ordering};

use crate::RingBufferMeta;
use crate::sequencers::Sequencer;
use crate::{ConsumerSeqGate, Cursor, PublisherSeqGate, Sequence, errors::TryClaimError};

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
    fn do_try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let last_consumed = Sequence::new(self.consumed.load(Ordering::Relaxed));
        let highest_to_claim = last_consumed + n;
        let next_claim = last_consumed + 1;

        if highest_to_claim <= self.max_published {
            return Ok((next_claim, highest_to_claim));
        }

        let max_published = self
            .producer_gate
            .max_published(next_claim, highest_to_claim);
        if max_published > self.max_published {
            self.max_published = max_published;
        }
        if max_published >= highest_to_claim {
            return Ok((next_claim, highest_to_claim));
        }

        let shutdown_sequence = self.producer_gate.shutdown_sequence();
        if !shutdown_sequence.is_shutdown_open() {
            // Producer has shutdown. Check if we've consumed all available data.
            if last_consumed >= shutdown_sequence {
                // All data drained, safe to disconnect
                return Err(TryClaimError::Shutdown);
            }
            // Still have data to consume, return Insufficient to allow draining
        }

        Err(TryClaimError::Insufficient {
            missing: highest_to_claim.value() - max_published.value(),
        })
    }

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
    fn try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        self.do_try_claim_n(n)
    }

    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.do_try_claim_n(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let last_claimed = Sequence::new(self.consumed.load(Ordering::Relaxed));
        let n = limit.min(self.ring_meta.buffer_size());
        let start_seq = last_claimed + 1;
        let highest_to_consume = last_claimed + n;

        if highest_to_consume > self.max_published {
            let max_published = self
                .producer_gate
                .max_published(start_seq, highest_to_consume);
            self.max_published = max_published;
        }

        if highest_to_consume <= self.max_published {
            return Ok((start_seq, highest_to_consume));
        }

        let shutdown_sequence = self.producer_gate.shutdown_sequence();
        let end_seq = if !shutdown_sequence.is_shutdown_open() {
            if last_claimed >= shutdown_sequence {
                return Err(TryClaimError::Shutdown);
            }

            let end_seq = shutdown_sequence.min(self.max_published);
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
    fn max_consumed(&self, _next_seq: crate::Sequence, _end_seq: crate::Sequence) -> Sequence {
        Sequence::new(self.consumed.load(Ordering::Acquire))
    }

    #[inline]
    fn is_disconnected(&self) -> bool {
        Sequence::new(self.consumed.load(Ordering::Acquire)).is_shutdown_open()
    }
}
