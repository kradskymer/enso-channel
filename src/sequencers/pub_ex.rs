use std::sync::{Arc, atomic::Ordering};

use crate::{
    ConsumerSeqGate, Cursor, PublisherSeqGate, RingBufferMeta, Sequence, errors::TryClaimError,
    sequencers::Sequencer,
};

pub struct ExclusivePublisherSequencer<C: ConsumerSeqGate> {
    claimed: Sequence,
    published: Arc<Cursor>,
    shutdown_sequence: Arc<Cursor>,
    max_available: Sequence,
    consumer_gate: Arc<C>,
    ring_meta: RingBufferMeta,
}

impl<C> ExclusivePublisherSequencer<C>
where
    C: ConsumerSeqGate,
{
    pub fn new(consumer_gate: Arc<C>, ring_meta: RingBufferMeta) -> Self {
        Self {
            claimed: Sequence::INIT,
            published: Arc::new(Cursor::INIT),
            shutdown_sequence: Arc::new(Cursor::new(Sequence::SHUTDOWN_OPEN)),
            max_available: Sequence::INIT,
            consumer_gate,
            ring_meta,
        }
    }

    #[inline]
    fn close(&self) {
        let last_published = self.published.load(Ordering::Acquire);
        let _ = self.shutdown_sequence.compare_exchange(
            Sequence::SHUTDOWN_OPEN.value(),
            last_published,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn publisher_gate(&self) -> ExclusivePubSeqGate {
        ExclusivePubSeqGate {
            published: self.published.clone(),
            shutdown_sequence: self.shutdown_sequence.clone(),
        }
    }
}

impl<C> Sequencer for ExclusivePublisherSequencer<C>
where
    C: ConsumerSeqGate,
{
    fn try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let capacity = self.ring_meta.buffer_size();
        if n > capacity {
            return Err(TryClaimError::Insufficient {
                missing: n - capacity,
            });
        }

        let highest_to_publish = self.claimed + n;
        let start_seq = self.claimed + 1;
        if highest_to_publish > self.max_available {
            let max_consumed = self
                .consumer_gate
                .max_consumed(start_seq, highest_to_publish);
            if max_consumed.is_shutdown_open() {
                return Err(TryClaimError::Shutdown);
            }

            let free_slots = self.ring_meta.available_slots(self.claimed, max_consumed);
            let max_available = self.claimed + free_slots;
            if max_available > self.max_available {
                self.max_available = max_available;
            }
        }
        if highest_to_publish <= self.max_available {
            if self.consumer_gate.is_disconnected() {
                return Err(TryClaimError::Shutdown);
            }
            self.claimed = highest_to_publish;
            Ok((start_seq, highest_to_publish))
        } else {
            let missing = highest_to_publish.value() - self.max_available.value();
            Err(TryClaimError::Insufficient { missing })
        }
    }

    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.try_claim_n(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let n = limit.min(self.ring_meta.buffer_size());
        let highest_to_publish = self.claimed + n;
        let start_seq = self.claimed + 1;
        if highest_to_publish > self.max_available {
            let max_consumed = self
                .consumer_gate
                .max_consumed(start_seq, highest_to_publish);
            if max_consumed.is_shutdown_open() {
                return Err(TryClaimError::Shutdown);
            }

            let free_slots = self.ring_meta.available_slots(self.claimed, max_consumed);
            let max_available = self.claimed + free_slots;
            if max_available > self.max_available {
                self.max_available = max_available;
            }
        }

        if start_seq > self.max_available {
            return Err(TryClaimError::Empty);
        }

        if self.consumer_gate.is_disconnected() {
            return Err(TryClaimError::Shutdown);
        }

        let available = (self.max_available.value() - self.claimed.value()).min(n);
        let end_seq = self.claimed + available;
        self.claimed = end_seq;
        Ok((start_seq, end_seq))
    }

    #[inline]
    fn commit(&self, seq: Sequence) {
        self.published.store(seq.value(), Ordering::Release);
    }

    #[inline]
    fn commit_range(&self, _start_seq: Sequence, end_seq: Sequence) {
        self.commit(end_seq);
    }
}

impl<C: ConsumerSeqGate> crate::sequencers::sealed::Sealed for ExclusivePublisherSequencer<C> {}

impl<C> Drop for ExclusivePublisherSequencer<C>
where
    C: ConsumerSeqGate,
{
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone)]
pub(crate) struct ExclusivePubSeqGate {
    published: Arc<Cursor>,
    shutdown_sequence: Arc<Cursor>,
}

impl PublisherSeqGate for ExclusivePubSeqGate {
    #[inline]
    fn max_published(&self, _next_seq: Sequence, _end_seq: Sequence) -> Sequence {
        Sequence::new(self.published.load(Ordering::Acquire))
    }

    #[inline]
    fn shutdown_sequence(&self) -> Sequence {
        Sequence::new(self.shutdown_sequence.load(Ordering::Acquire))
    }
}
