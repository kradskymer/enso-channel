use std::sync::atomic::{fence, AtomicU32, Ordering};

use crate::sequencers::SlotState;
use crate::{RingBufferMeta, Sequence};

/// Shared abstraction for per-slot state tracking in the ring buffer.
pub(crate) trait SlotStateGroup: Send + Sync {
    /// Marks the inclusive range `[start, end]` as completed/published. Implementers
    /// must translate absolute sequence numbers to slot indexes and handle lap
    /// tracking transparently so the caller can make slots available for the
    /// opposite side of the ring (publisher vs consumer).
    fn publish_range(&self, start: Sequence, end: Sequence);

    /// Marks the sequence `seq` as completed/published.
    fn publish(&self, seq: Sequence);

    /// Scans from `start` toward `end` and returns the last *contiguous* sequence
    /// that is currently available together with a shutdown flag.
    ///
    /// The returned [`SlotState::sequence()`] is `start - 1` when `start` itself
    /// is unavailable.  The [`SlotState::is_shutdown()`] flag is true when the
    /// opposing side has marked a slot within the scan range as shut down.
    ///
    /// "Available" is context dependent:
    /// - On the publisher side it means consumers have freed the slot.
    /// - On the consumer side it means publishers have published the slot.
    fn scan_available_until(&self, start: Sequence, end: Sequence) -> SlotState;
}

/// Per-slot state tracking using `u32` lap flags.
///
/// ## Invariant: bounded lag prevents lap-wrap ambiguity
///
/// This implementation stores an `AtomicU32` per slot to record a *lap flag* derived from the
/// absolute sequence number:
///
/// - The slot index is computed from the low bits of the sequence.
/// - The stored flag is derived from the lap counter (sequence >> log2(capacity)), stored as a
///   `u32` (with wrapping arithmetic).
///
/// A natural question is whether `u32` wrap-around could cause false positives (treating a slot
/// from an old lap as available in a new lap). In this crate, such a collision is unobservable
/// under the core ring-buffer invariant:
///
/// - Publishers/consumers never advance more than one full ring ahead of the opposing side for a
///   given slot index. Equivalently, there is a bounded lag between the “slowest” and “fastest”
///   cursor so that at any time, for each slot index, only one relevant lap is in-flight.
///
/// Reaching a `u32` lap collision for the same slot index would require the producer to advance by
/// $2^{32}$ laps without the opposing side advancing enough to keep the slot “in-flight” bounded,
/// which violates the backpressure/claim protocol enforced by the sequencers.
///
/// This is why the lap flag is safe even on long-running systems: the protocol prevents the state
/// machine from ever having to distinguish between laps separated by $2^{32}$ for the same slot.
pub(crate) struct U32SlotStates {
    states: Box<[AtomicU32]>,
    index_shift: i64,
    ring_meta: RingBufferMeta,
    flag_adjustment: u32,
}

/// Mask that reserves the highest bit (bit 31) for shutdown signaling.
/// The lower 31 bits carry the lap flag.
const MASK: u32 = (1 << 31) - 1;

/// The shutdown bit within the per-slot `u32` state.
const SHUTDOWN_BIT: u32 = 1 << 31;

impl U32SlotStates {
    pub(crate) fn new_all_empty(ring_meta: RingBufferMeta) -> Self {
        let capacity = ring_meta.buffer_size() as usize;
        let states = (0..capacity).map(|_| AtomicU32::new(0)).collect();

        let index_shift = capacity.trailing_zeros() as i64;
        Self {
            states,
            index_shift,
            ring_meta,
            flag_adjustment: 1,
        }
    }

    fn slot_index(&self, seq: Sequence) -> usize {
        self.ring_meta.index_of_seq(seq)
    }

    fn available_flag(&self, seq: Sequence) -> u32 {
        let lap = (seq.value() >> self.index_shift) as u32;
        lap.wrapping_add(self.flag_adjustment) & MASK
    }

    fn slot_state(&self, seq: Sequence) -> (usize, u32) {
        let index = self.slot_index(seq);
        let flag = self.available_flag(seq);
        (index, flag)
    }

    /// Sets the shutdown bit (bit 31) on the slot for `seq`.
    #[inline]
    pub(crate) fn mark_shutdown(&self, seq: Sequence) {
        let index = self.slot_index(seq);
        self.states[index].fetch_or(SHUTDOWN_BIT, Ordering::Release);
    }

    /// Scans from `start` toward `end` and returns the last *contiguous*
    /// sequence whose lap flag matches, **without** shutdown detection.
    ///
    /// This internal helper is used during shutdown to discover the
    /// last-committed / last-published boundary before setting the shutdown
    /// bit.  Callers must not use it on the hot path.
    pub(crate) fn scan_last_available(&self, start: Sequence, end: Sequence) -> Sequence {
        debug_assert!(start <= end, "scan range must be ordered");
        let mut last_available = start.value() - 1;
        for seq_value in start.value()..=end.value() {
            let seq = Sequence::new(seq_value);
            let (index, flag) = self.slot_state(seq);
            if self.states[index].load(Ordering::Relaxed) & MASK != flag {
                break;
            }
            last_available = seq_value;
        }
        if last_available >= start.value() {
            fence(Ordering::Acquire);
        }
        Sequence::new(last_available)
    }
}

impl SlotStateGroup for U32SlotStates {
    #[inline]
    fn publish_range(&self, start: Sequence, end: Sequence) {
        debug_assert!(start <= end, "publish_range expects start <= end");
        fence(Ordering::Release);
        for seq_value in start.value()..=end.value() {
            let seq = Sequence::new(seq_value);
            let (index, flag) = self.slot_state(seq);
            self.states[index].store(flag, Ordering::Relaxed);
        }
    }

    #[inline]
    fn scan_available_until(&self, start: Sequence, end: Sequence) -> SlotState {
        debug_assert!(start <= end, "scan range must be ordered");
        let mut last_available = start.value() - 1;
        let mut shutdown = false;
        for seq_value in start.value()..=end.value() {
            let seq = Sequence::new(seq_value);
            let (index, flag) = self.slot_state(seq);
            let raw = self.states[index].load(Ordering::Relaxed);
            shutdown |= (raw & SHUTDOWN_BIT) != 0;
            if (raw & MASK) != flag {
                break;
            }
            last_available = seq_value;
        }
        // Ensure readers observe slot writes for all sequences <= last_available.
        if last_available >= start.value() {
            fence(Ordering::Acquire);
        }
        SlotState::new(Sequence::new(last_available), shutdown)
    }

    #[inline]
    fn publish(&self, seq: Sequence) {
        fence(Ordering::Release);
        let (index, flag) = self.slot_state(seq);
        self.states[index].store(flag, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::{SlotStateGroup, U32SlotStates};
    use crate::{RingBufferMeta, Sequence};

    fn run_slot_state_group_suite<G: SlotStateGroup>(make: impl Fn(RingBufferMeta) -> G) {
        let ring = RingBufferMeta::new(8);
        let states = make(ring);

        // Helper: scan and unwrap sequence (tests with no shutdown).
        let scan_seq = |s: &G, start, end| s.scan_available_until(start, end).sequence();

        // Empty scans should return start - 1.
        assert_eq!(
            scan_seq(&states, Sequence::new(0), Sequence::new(0)),
            Sequence::new(-1)
        );
        assert_eq!(
            scan_seq(&states, Sequence::new(5), Sequence::new(7)),
            Sequence::new(4)
        );

        // Publishing a single sequence should make it available.
        states.publish(Sequence::new(0));
        assert_eq!(
            scan_seq(&states, Sequence::new(0), Sequence::new(0)),
            Sequence::new(0)
        );

        // Publishing with a hole should stop at the last contiguous sequence.
        states.publish(Sequence::new(2));
        assert_eq!(
            scan_seq(&states, Sequence::new(0), Sequence::new(2)),
            Sequence::new(0)
        );

        // Filling the hole makes the range contiguous.
        states.publish(Sequence::new(1));
        assert_eq!(
            scan_seq(&states, Sequence::new(0), Sequence::new(2)),
            Sequence::new(2)
        );

        // publish_range should make the entire inclusive range available.
        states.publish_range(Sequence::new(3), Sequence::new(5));
        assert_eq!(
            scan_seq(&states, Sequence::new(0), Sequence::new(5)),
            Sequence::new(5)
        );

        // Test scanning a subset of available range
        assert_eq!(
            scan_seq(&states, Sequence::new(3), Sequence::new(4)),
            Sequence::new(4)
        );

        // Verify no false shutdown in normal operation.
        let result = states.scan_available_until(Sequence::new(0), Sequence::new(5));
        assert!(!result.is_shutdown());
    }

    fn run_lap_handling_suite<G: SlotStateGroup>(make: impl Fn(RingBufferMeta) -> G) {
        let ring = RingBufferMeta::new(4); // Small ring for easier wrapping
        let states = make(ring);

        let scan_seq = |s: &G, start, end| s.scan_available_until(start, end).sequence();

        // Publish seq 0 (lap 0, slot 0).
        states.publish(Sequence::new(0));
        assert_eq!(
            scan_seq(&states, Sequence::new(0), Sequence::new(0)),
            Sequence::new(0)
        );

        // Publish seq 4 (lap 1, same slot 0). This should overwrite slot 0.
        states.publish(Sequence::new(4));

        // Seq 0 should no longer be available.
        assert_eq!(
            scan_seq(&states, Sequence::new(0), Sequence::new(0)),
            Sequence::new(-1)
        );
        // Seq 4 should be available.
        assert_eq!(
            scan_seq(&states, Sequence::new(4), Sequence::new(4)),
            Sequence::new(4)
        );

        // Test publish_range causing wrap-around overwrite
        // Current state: Slot 0 has seq 4. Slots 1,2,3 are empty (seq -1 effectively).

        // Publish range 2..5.
        states.publish_range(Sequence::new(2), Sequence::new(5));

        // Check availability
        assert_eq!(
            scan_seq(&states, Sequence::new(2), Sequence::new(3)),
            Sequence::new(3)
        );

        assert_eq!(
            scan_seq(&states, Sequence::new(4), Sequence::new(5)),
            Sequence::new(5)
        );

        // Seq 0..1 should NOT be available (overwritten by 4, 5)
        assert_eq!(
            scan_seq(&states, Sequence::new(0), Sequence::new(1)),
            Sequence::new(-1)
        );

        // Mixed scan: 2..5.
        assert_eq!(
            scan_seq(&states, Sequence::new(2), Sequence::new(5)),
            Sequence::new(5)
        );
    }

    #[test]
    fn shutdown_bit_detected_by_scan() {
        let ring = RingBufferMeta::new(4);
        let states = U32SlotStates::new_all_empty(ring);

        // Publish 0..2, then mark slot 3 (seq 3) as shutdown.
        states.publish_range(Sequence::new(0), Sequence::new(2));
        states.mark_shutdown(Sequence::new(3));

        let result = states.scan_available_until(Sequence::new(0), Sequence::new(3));
        assert_eq!(result.sequence(), Sequence::new(2));
        assert!(result.is_shutdown());
    }

    #[test]
    fn shutdown_on_published_slot_still_detected() {
        let ring = RingBufferMeta::new(4);
        let states = U32SlotStates::new_all_empty(ring);

        // Publish 0..3, mark shutdown on seq 3 itself.
        states.publish_range(Sequence::new(0), Sequence::new(3));
        states.mark_shutdown(Sequence::new(3));

        let result = states.scan_available_until(Sequence::new(0), Sequence::new(3));
        assert_eq!(result.sequence(), Sequence::new(3));
        assert!(result.is_shutdown());
    }

    macro_rules! generate_slot_state_tests {
        ($impl_type:ty, $prefix:literal, $constructor:expr) => {
            paste::paste! {

            #[test]
            fn [< $prefix _obeys_slot_state_group_contract>]() {
                $crate::slot_states::tests::run_slot_state_group_suite($constructor);
            }

            #[test]
            fn [< $prefix _handles_lap_wrapping_correctly>]() {
                $crate::slot_states::tests::run_lap_handling_suite($constructor);
            }
            }
        };
    }

    // Use the macro for U32SlotStates
    generate_slot_state_tests!(U32SlotStates, "u32_new_empty", |ring| {
        U32SlotStates::new_all_empty(ring)
    });
}
