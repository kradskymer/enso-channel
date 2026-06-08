use std::sync::{atomic::Ordering, Arc};

use super::super::RingBufferMeta;
use crate::sequencers::{ConsumerBarrier, ProducerBarrier, Sequencer, SlotState};
use crate::{
    errors::TryClaimError,
    slot_states::{SlotStateGroup, U32SlotStates},
    Cursor, Sequence,
};

/// The highest bit of the `claimed` cursor signals shutdown.
/// When set, all future `try_claim` / `try_claim_at_most` calls return `Shutdown`.
///
/// Because `Sequence::INIT = -1` has all bits set (including bit 63),
/// we cannot check the bit in isolation.  Instead, any negative claimed
/// value other than `-1` signals shutdown.
const SHUTDOWN_MASK: i64 = i64::MIN;
/// Mask to clear the shutdown bit and recover the actual sequence value.
const VALUE_MASK: i64 = i64::MAX;

/// Returns `true` when `raw` indicates the publisher side is shut down.
///
/// The cursor stores `Sequence::INIT = -1` by default, which happens to
/// have the sign bit set.  We therefore treat only *other* negative values
/// (`< -1`) as the shutdown signal.
#[inline]
fn has_shutdown(raw: i64) -> bool {
    raw < 0 && raw != -1
}

#[derive(Clone)]
pub(crate) struct MultiPublisherSequencer<C: ConsumerBarrier> {
    shared_state: Arc<SharedState>,
    max_available: Sequence,
    consumer_gate: Arc<C>,
    ring_meta: RingBufferMeta,
}

struct SharedState {
    claimed: Arc<Cursor>,
    slot_states: Arc<U32SlotStates>,
    buffer_size: i64,
}

impl SharedState {
    fn new(ring_meta: RingBufferMeta) -> Self {
        Self {
            claimed: Arc::new(Cursor::default()),
            slot_states: Arc::new(U32SlotStates::new_all_empty(ring_meta)),
            buffer_size: ring_meta.buffer_size(),
        }
    }
}

impl SharedState {
    #[inline]
    fn close(&self) {
        // Compute a shutdown boundary that is based on what is *actually published*
        // (as tracked by `slot_states`), not just what was claimed.
        //
        // We scan only within the ring window `[last_claimed - (capacity - 1), last_claimed]`.
        // A consumer cannot lag by more than `capacity - 1` sequences because publishers are
        // backpressured by the ring buffer.
        let raw = self.claimed.load(Ordering::Acquire);
        // Strip the shutdown bit only when it is actually set;
        // otherwise the raw value may be `Sequence::INIT = -1`.
        let last_claimed = if has_shutdown(raw) {
            Sequence::new(raw & VALUE_MASK)
        } else {
            Sequence::new(raw)
        };

        let last_published = if last_claimed < Sequence::new(0) {
            // No items were ever claimed.
            last_claimed
        } else {
            let window_start_value = (last_claimed.value() - (self.buffer_size - 1)).max(0);
            let window_start = Sequence::new(window_start_value);
            self.slot_states
                .scan_last_available(window_start, last_claimed)
        };

        // Mark the first unpublished slot as the shutdown boundary.
        // This makes the shutdown visible to consumers via the scan.
        self.slot_states.mark_shutdown(last_published + 1);
    }
}

impl Drop for SharedState {
    fn drop(&mut self) {
        self.close();
    }
}

impl<C: ConsumerBarrier> MultiPublisherSequencer<C> {
    pub(crate) fn new(consumer_gate: Arc<C>, ring_meta: RingBufferMeta) -> Self {
        Self {
            shared_state: Arc::new(SharedState::new(ring_meta)),
            max_available: Sequence::INIT,
            consumer_gate,
            ring_meta,
        }
    }

    #[inline]
    pub(crate) fn publisher_gate(&self) -> MultiPubSeqGate {
        MultiPubSeqGate {
            slot_states: self.shared_state.slot_states.clone(),
        }
    }
}

impl<C: ConsumerBarrier> Sequencer for MultiPublisherSequencer<C> {
    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.try_claim_at_most(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let n = limit.min(self.ring_meta.buffer_size());

        let raw = self.shared_state.claimed.load(Ordering::Relaxed);
        let mut last_claimed = Sequence::new(raw);
        loop {
            if has_shutdown(last_claimed.value()) {
                return Err(TryClaimError::Shutdown);
            }
            let start_seq = last_claimed + 1;

            let consumed = self.consumer_gate.max_consumed();
            if consumed.is_shutdown() {
                return Err(TryClaimError::Shutdown);
            }
            let max_consumed = consumed.sequence();

            let available_capacity = self.ring_meta.available_slots(last_claimed, max_consumed);
            self.max_available = last_claimed + available_capacity;

            // None available
            if start_seq > self.max_available {
                return Err(TryClaimError::Empty);
            }

            let available = (self.max_available.value() - last_claimed.value()).min(n);
            let highest_to_publish = last_claimed + available;

            let expected = last_claimed.value();
            match self.shared_state.claimed.compare_exchange(
                expected,
                highest_to_publish.value(),
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Ok((start_seq, highest_to_publish));
                }
                Err(new_value) => {
                    last_claimed = Sequence::new(new_value);
                }
            }
        }
    }

    #[inline]
    fn commit(&self, seq: Sequence) {
        self.shared_state.slot_states.publish(seq);
    }

    #[inline]
    fn commit_range(&self, start_seq: Sequence, end_seq: Sequence) {
        self.shared_state
            .slot_states
            .publish_range(start_seq, end_seq);
    }

    #[inline]
    fn terminate(&self) {
        let raw = self.shared_state.claimed.load(Ordering::Acquire);
        if has_shutdown(raw) {
            return;
        }
        // Mark shutdown in slot states FIRST so consumers observe it
        // before (or concurrently with) other senders seeing the claimed bit.
        self.shared_state.close();
        // Then block other senders.
        self.shared_state
            .claimed
            .store(raw | SHUTDOWN_MASK, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct MultiPubSeqGate {
    slot_states: Arc<U32SlotStates>,
}

impl ProducerBarrier for MultiPubSeqGate {
    #[inline]
    fn max_published(&self, next_seq: Sequence, end_seq: Sequence) -> SlotState {
        self.slot_states.scan_available_until(next_seq, end_seq)
    }
}
