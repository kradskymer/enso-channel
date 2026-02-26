use std::sync::{Arc, atomic::Ordering};

use super::super::RingBufferMeta;
use crate::sequencers::Sequencer;
use crate::{
    ConsumerSeqGate, Cursor, PublisherSeqGate, Sequence,
    errors::TryClaimError,
    slot_states::{SlotStateGroup, U32SlotStates},
};

#[derive(Clone)]
pub(crate) struct MultiPublisherSequencer<C: ConsumerSeqGate> {
    shared_state: Arc<SharedState>,
    max_available: Sequence,
    consumer_gate: Arc<C>,
    ring_meta: RingBufferMeta,
}

struct SharedState {
    claimed: Arc<Cursor>,
    slot_states: Arc<U32SlotStates>,
    shutdown_sequence: Arc<Cursor>,
    buffer_size: i64,
}

impl SharedState {
    fn new(ring_meta: RingBufferMeta) -> Self {
        Self {
            claimed: Arc::new(Cursor::INIT),
            slot_states: Arc::new(U32SlotStates::new_all_empty(ring_meta)),
            shutdown_sequence: Arc::new(Cursor::new(Sequence::SHUTDOWN_OPEN)),
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
        let last_claimed = Sequence::new(self.claimed.load(Ordering::Acquire));

        let last_published = if last_claimed < Sequence::new(0) {
            // No items were ever claimed.
            last_claimed
        } else {
            let window_start_value = (last_claimed.value() - (self.buffer_size - 1)).max(0);
            let window_start = Sequence::new(window_start_value);
            self.slot_states
                .scan_available_until(window_start, last_claimed)
        };

        let _ = self.shutdown_sequence.compare_exchange(
            Sequence::SHUTDOWN_OPEN.value(),
            last_published.value(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

impl Drop for SharedState {
    fn drop(&mut self) {
        self.close();
    }
}

impl<C: ConsumerSeqGate> MultiPublisherSequencer<C> {
    pub(crate) fn new(consumer_gate: Arc<C>, ring_meta: RingBufferMeta) -> Self {
        Self {
            shared_state: Arc::new(SharedState::new(ring_meta)),
            max_available: Sequence::INIT,
            consumer_gate,
            ring_meta,
        }
    }

    fn do_try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let mut last_claimed = Sequence::new(self.shared_state.claimed.load(Ordering::Relaxed));
        loop {
            let highest_to_publish = last_claimed + n;
            let start_seq = last_claimed + 1;

            if highest_to_publish <= self.max_available {
                if self.consumer_gate.is_disconnected() {
                    return Err(TryClaimError::Shutdown);
                }

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
                        let new_seq = Sequence::new(new_value);
                        last_claimed = new_seq;
                        continue;
                    }
                }
            }

            let max_consumed = self
                .consumer_gate
                .max_consumed(start_seq, highest_to_publish);
            if max_consumed.is_shutdown_open() {
                return Err(TryClaimError::Shutdown);
            }

            let available_capacity = self.ring_meta.available_slots(last_claimed, max_consumed);
            self.max_available = last_claimed + available_capacity;
            if available_capacity < n {
                return Err(TryClaimError::Insufficient {
                    missing: n - available_capacity,
                });
            }
        }
    }

    #[inline]
    fn do_commit(&self, seq: Sequence) {
        self.shared_state.slot_states.publish(seq);
    }

    #[inline]
    fn do_commit_range(&self, start_seq: Sequence, end_seq: Sequence) {
        self.shared_state
            .slot_states
            .publish_range(start_seq, end_seq);
    }

    #[inline]
    pub(crate) fn publisher_gate(&self) -> MultiPubSeqGate {
        MultiPubSeqGate {
            slot_states: self.shared_state.slot_states.clone(),
            shutdown_sequence: self.shared_state.shutdown_sequence.clone(),
        }
    }
}

impl<C: ConsumerSeqGate> Sequencer for MultiPublisherSequencer<C> {
    fn try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        self.do_try_claim_n(n)
    }

    fn try_claim(&mut self) -> Result<Sequence, TryClaimError> {
        self.do_try_claim_n(1).map(|(_, end_seq)| end_seq)
    }

    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError> {
        let n = limit.min(self.ring_meta.buffer_size());

        let mut last_claimed = Sequence::new(self.shared_state.claimed.load(Ordering::Relaxed));
        loop {
            let highest_to_publish = last_claimed + n;
            let start_seq = last_claimed + 1;

            if highest_to_publish > self.max_available {
                let max_consumed = self
                    .consumer_gate
                    .max_consumed(start_seq, highest_to_publish);
                if max_consumed.is_shutdown_open() {
                    return Err(TryClaimError::Shutdown);
                }

                let available_capacity = self.ring_meta.available_slots(last_claimed, max_consumed);
                self.max_available = last_claimed + available_capacity;
            }

            // None available
            if start_seq > self.max_available {
                return Err(TryClaimError::Empty);
            }

            if self.consumer_gate.is_disconnected() {
                return Err(TryClaimError::Shutdown);
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
        self.do_commit(seq);
    }

    #[inline]
    fn commit_range(&self, start_seq: Sequence, end_seq: Sequence) {
        self.do_commit_range(start_seq, end_seq);
    }
}

impl<C: ConsumerSeqGate> crate::sequencers::sealed::Sealed for MultiPublisherSequencer<C> {}

#[derive(Clone)]
pub(crate) struct MultiPubSeqGate {
    slot_states: Arc<U32SlotStates>,
    shutdown_sequence: Arc<Cursor>,
}

impl PublisherSeqGate for MultiPubSeqGate {
    #[inline]
    fn max_published(&self, next_seq: Sequence, end_seq: Sequence) -> Sequence {
        self.slot_states.scan_available_until(next_seq, end_seq)
    }

    #[inline]
    fn shutdown_sequence(&self) -> Sequence {
        self.shutdown_sequence.load(Ordering::Acquire).into()
    }
}
