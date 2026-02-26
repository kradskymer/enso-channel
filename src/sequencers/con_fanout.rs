use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use crate::{
    ConsumerSeqGate, Cursor, PublisherSeqGate, RingBufferMeta, Sequence, errors::TryClaimError,
    sequencers::Sequencer,
};

// Removed ConsumerOps adapter

pub(crate) struct FanoutConsumerSequencer<P: PublisherSeqGate> {
    consumed: Arc<Cursor>,
    connected: Arc<AtomicUsize>,
    max_published: Sequence,
    producer_gate: Arc<P>,
    ring_meta: RingBufferMeta,
}

impl<P: PublisherSeqGate> FanoutConsumerSequencer<P> {
    pub fn new(
        producer_gate: Arc<P>,
        consumed: Arc<Cursor>,
        connected: Arc<AtomicUsize>,
        ring_meta: RingBufferMeta,
    ) -> Self {
        Self {
            consumed,
            connected,
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
        let max_published = self
            .producer_gate
            .max_published(start_seq, highest_to_claim);
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

    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        Sequencer::try_claim_n(self, 1).map(|(_, end_seq)| end_seq)
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

        // If this was the last consumer, publishers must observe a disconnected state.
        // Saturation is not expected (each consumer sequencer is dropped once).
        let prev = self.connected.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(prev > 0, "fanout connected counter underflow");
    }
}

#[derive(Clone)]
pub(crate) struct FanoutConSeqGate<const N: usize> {
    consumed: [Arc<Cursor>; N],
    connected: Arc<AtomicUsize>,
}

impl<const N: usize> FanoutConSeqGate<N> {
    pub(crate) fn new(consumed: [Arc<Cursor>; N]) -> Self {
        Self {
            consumed,
            connected: Arc::new(AtomicUsize::new(N)),
        }
    }

    pub(crate) fn disconnect_counter(&self) -> Arc<AtomicUsize> {
        self.connected.clone()
    }
}

impl<const N: usize> ConsumerSeqGate for FanoutConSeqGate<N> {
    fn max_consumed(&self, _next_seq: Sequence, _end_seq: Sequence) -> Sequence {
        if self.connected.load(Ordering::Acquire) == 0 {
            return Sequence::SHUTDOWN_OPEN;
        }

        let mut min_consumed: Option<Sequence> = None;
        for i in 0..N {
            let value = self.consumed[i].load(Ordering::Acquire);
            let seq = Sequence::new(value);
            if seq.is_shutdown_open() {
                // Consumer has been dropped; it must not gate the publisher.
                continue;
            }
            min_consumed = Some(match min_consumed {
                Some(current) => current.min(seq),
                None => seq,
            });
        }

        let Some(min_consumed) = min_consumed else {
            // All consumers have been dropped; publishers must observe a disconnected state.
            return Sequence::SHUTDOWN_OPEN;
        };

        min_consumed
    }

    #[inline]
    fn is_disconnected(&self) -> bool {
        self.connected.load(Ordering::Acquire) == 0
    }
}
