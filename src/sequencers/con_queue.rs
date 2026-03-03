use std::sync::{atomic::Ordering, Arc};

use crate::sequencers::{ConsumerSeqGate, PublisherSeqGate, Sequencer};
use crate::{
    errors::TryClaimError,
    slot_states::{SlotStateGroup, U32SlotStates},
    Cursor, RingBufferMeta, Sequence,
};

/// Multi-consumer work-queue consumer sequencer.
///
/// Semantics:
/// - Consumers *compete* to claim the next available sequences (each item is delivered once).
/// - Consumption is committed through per-slot state to reduce contention on the commit path.
#[derive(Clone)]
pub(crate) struct QueuedConsumerSequencer<P: PublisherSeqGate> {
    shared_state: Arc<QueuedSharedState>,
    max_published: Sequence,
    publisher_gate: Arc<P>,
    ring_meta: RingBufferMeta,
}

struct QueuedSharedState {
    /// Highest sequence claimed by any consumer (claim cursor).
    claimed: Arc<Cursor>,
    /// Per-slot consumed/available flags (commit path).
    consumed_states: Arc<U32SlotStates>,
}

/// Wiring helper for work-queue consumer topology.
///
/// This keeps the internal shared-state details private to this module while
/// allowing topologies to:
/// 1) build the publisher-facing consumer gate, then
/// 2) build the consumer sequencer once the publisher gate exists.
pub(crate) struct QueuedConsumerWiring {
    shared_state: Arc<QueuedSharedState>,
}

impl QueuedConsumerWiring {
    pub(crate) fn new(ring_meta: RingBufferMeta) -> (Arc<QueuedConSeqGate>, Self) {
        let shared_state = Arc::new(QueuedSharedState::new(ring_meta));
        let consumer_gate = Arc::new(QueuedConSeqGate::new(shared_state.clone(), ring_meta));
        (consumer_gate, Self { shared_state })
    }

    pub(crate) fn build_consumer<P: PublisherSeqGate>(
        self,
        publisher_gate: Arc<P>,
        ring_meta: RingBufferMeta,
    ) -> QueuedConsumerSequencer<P> {
        QueuedConsumerSequencer::new(publisher_gate, self.shared_state, ring_meta)
    }
}

impl QueuedSharedState {
    pub(crate) fn new(ring_meta: RingBufferMeta) -> Self {
        Self {
            claimed: Arc::new(Cursor::INIT),
            consumed_states: Arc::new(U32SlotStates::new_all_available(ring_meta)),
        }
    }
}

impl Drop for QueuedSharedState {
    fn drop(&mut self) {
        // Encode consumer disconnect for publishers via the claimed cursor sentinel.
        // After disconnect, publishers must stop producing.
        self.claimed
            .store(Sequence::SHUTDOWN_OPEN.value(), Ordering::Release);
    }
}

impl<P: PublisherSeqGate> QueuedConsumerSequencer<P> {
    fn new(
        publisher_gate: Arc<P>,
        shared_state: Arc<QueuedSharedState>,
        ring_meta: RingBufferMeta,
    ) -> Self {
        Self {
            shared_state,
            max_published: Sequence::INIT,
            publisher_gate,
            ring_meta,
        }
    }

    fn do_try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let mut last_claimed = Sequence::new(self.shared_state.claimed.load(Ordering::Relaxed));
        loop {
            let highest_to_claim = last_claimed + n;
            let next_claim = last_claimed + 1;

            if highest_to_claim > self.max_published {
                let max_published = self
                    .publisher_gate
                    .max_published(next_claim, highest_to_claim);
                if max_published > self.max_published {
                    self.max_published = max_published;
                }
                if max_published < highest_to_claim {
                    let shutdown_sequence = self.publisher_gate.shutdown_sequence();
                    if !shutdown_sequence.is_shutdown_open() {
                        // Producer has shutdown. Check if we've consumed all available data.
                        if last_claimed >= shutdown_sequence {
                            // All data drained, safe to disconnect
                            return Err(TryClaimError::Shutdown);
                        }
                        // Still have data to consume, return Insufficient to allow draining
                    }

                    return Err(TryClaimError::Insufficient {
                        missing: highest_to_claim.value() - max_published.value(),
                    });
                }
            }

            match self.shared_state.claimed.compare_exchange(
                last_claimed.value(),
                highest_to_claim.value(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok((next_claim, highest_to_claim)),
                Err(observed) => {
                    last_claimed = Sequence::new(observed);
                }
            }
        }
    }
}

impl<P: PublisherSeqGate> Sequencer for QueuedConsumerSequencer<P> {
    fn try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        self.do_try_claim_n(n)
    }

    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.do_try_claim_n(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let mut last_claimed = Sequence::new(self.shared_state.claimed.load(Ordering::Relaxed));
        let n = limit.min(self.ring_meta.buffer_size());
        loop {
            let start_seq = last_claimed + 1;
            let highest_to_consume = last_claimed + n;

            // update max_published cached value
            if highest_to_consume > self.max_published {
                let max_published = self
                    .publisher_gate
                    .max_published(start_seq, highest_to_consume);
                self.max_published = max_published;
            }

            // All requested are available
            if highest_to_consume <= self.max_published {
                let expected = last_claimed.value();
                let end_seq = highest_to_consume;
                match self.shared_state.claimed.compare_exchange(
                    expected,
                    end_seq.value(),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => return Ok((start_seq, end_seq)),
                    Err(observed) => {
                        last_claimed = Sequence::new(observed);
                        continue;
                    }
                }
            }

            let shutdown_sequence = self.publisher_gate.shutdown_sequence();
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

            // Atomically advance the claimed cursor to the computed end sequence.
            // This mirrors the full-claim path above and prevents returning a claimed
            // range without actually reserving it (which caused duplicates / stale state
            // in single-threaded tests).
            let expected = last_claimed.value();
            match self.shared_state.claimed.compare_exchange(
                expected,
                end_seq.value(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok((start_seq, end_seq)),
                Err(observed) => {
                    last_claimed = Sequence::new(observed);
                    continue;
                }
            }
        }
    }

    #[inline]
    fn commit(&self, seq: Sequence) {
        self.shared_state.consumed_states.publish(seq);
    }

    #[inline]
    fn commit_range(&self, start_seq: Sequence, end_seq: Sequence) {
        self.shared_state
            .consumed_states
            .publish_range(start_seq, end_seq);
    }
}

impl<P: PublisherSeqGate> crate::sequencers::sealed::Sealed for QueuedConsumerSequencer<P> {}

#[derive(Clone)]
pub(crate) struct QueuedConSeqGate {
    consumed_states: Arc<U32SlotStates>,
    claimed: Arc<Cursor>,
    capacity: i64,
}

impl QueuedConSeqGate {
    fn new(shared_state: Arc<QueuedSharedState>, ring_meta: RingBufferMeta) -> Self {
        Self {
            consumed_states: shared_state.consumed_states.clone(),
            claimed: shared_state.claimed.clone(),
            capacity: ring_meta.buffer_size(),
        }
    }
}

impl ConsumerSeqGate for QueuedConSeqGate {
    fn max_consumed(&self, next_seq: Sequence, end_seq: Sequence) -> Sequence {
        if self.claimed.load(Ordering::Acquire) == Sequence::SHUTDOWN_OPEN.value() {
            return Sequence::SHUTDOWN_OPEN;
        }

        // To determine if sequences [next_seq, end_seq] can be written, we need to check
        // if the previous occupants of those slots have been consumed.
        // Previous occupant of seq S is S - capacity.

        let prev_start = next_seq - self.capacity;
        let prev_end = end_seq - self.capacity;

        // Scan the previous lap's sequences to see which have been consumed.
        // The result tells us the previous occupants that are free.
        // We translate this back to current lap's interpretation.

        // Translate: if previous seq P was consumed, current seq P + capacity can be written.
        // But available_slots expects max_consumed in terms of total consumed,
        // not in terms of slots freed.
        // Actually, the return value is used as: free_slots = capacity - (claimed - max_consumed)
        // We want: free_slots = number of slots that have been freed
        // If prev seqs [prev_start, max_prev_consumed] are consumed,
        // then (max_prev_consumed - prev_start + 1) slots are free.
        // But we need to express this as max_consumed for the available_slots formula.
        //
        // Working backwards:
        // free_slots = max_prev_consumed - prev_start + 1 = max_prev_consumed - (next_seq - capacity) + 1
        // We need: capacity - (claimed - max_consumed) = max_prev_consumed - next_seq + capacity + 1
        // claimed = next_seq - 1 (when calling for single item)
        // capacity - (next_seq - 1 - max_consumed) = max_prev_consumed - next_seq + capacity + 1
        // capacity - next_seq + 1 + max_consumed = max_prev_consumed - next_seq + capacity + 1
        // max_consumed = max_prev_consumed
        //
        // So we can just return max_prev_consumed directly!
        self.consumed_states
            .scan_available_until(prev_start, prev_end)
    }

    #[inline]
    fn is_disconnected(&self) -> bool {
        self.claimed.load(Ordering::Acquire) == Sequence::SHUTDOWN_OPEN.value()
    }
}
