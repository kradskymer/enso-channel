use std::sync::{atomic::Ordering, Arc};

use crate::sequencers::{ConsumerSeqGate, PublisherSeqGate, Sequencer, SlotState};
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
    buffer_size: i64,
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
            buffer_size: ring_meta.buffer_size(),
        }
    }
}

impl Drop for QueuedSharedState {
    fn drop(&mut self) {
        // Publishers scan the *previous* lap in `max_consumed`
        // (prev_seq = next_seq - capacity), so a single-slot shutdown marker
        // is invisible if the publisher's query lands on a different slot
        // index.  Mark every slot to guarantee any scan detects the
        // disconnect.  O(capacity) is acceptable — this is the cold drop path
        // of the last consumer.
        for i in 0..(self.buffer_size as usize) {
            self.consumed_states.mark_shutdown_at_index(i);
        }
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
}

impl<P: PublisherSeqGate> Sequencer for QueuedConsumerSequencer<P> {
    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.try_claim_at_most(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let mut last_claimed = Sequence::new(self.shared_state.claimed.load(Ordering::Relaxed));
        let n = limit.min(self.ring_meta.buffer_size());
        loop {
            let start_seq = last_claimed + 1;
            let highest_to_consume = last_claimed + n;

            // update max_published cached value
            if highest_to_consume > self.max_published {
                let state = self
                    .publisher_gate
                    .max_published(start_seq, highest_to_consume);
                self.max_published = state.sequence();
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

            // Re-read max_published with shutdown awareness.
            let state = self
                .publisher_gate
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
    capacity: i64,
}

impl QueuedConSeqGate {
    fn new(shared_state: Arc<QueuedSharedState>, ring_meta: RingBufferMeta) -> Self {
        Self {
            consumed_states: shared_state.consumed_states.clone(),
            capacity: ring_meta.buffer_size(),
        }
    }
}

impl ConsumerSeqGate for QueuedConSeqGate {
    fn max_consumed(&self, next_seq: Sequence, end_seq: Sequence) -> SlotState {
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
}
