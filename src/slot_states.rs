use std::sync::atomic::{AtomicU32, Ordering, fence};

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
    /// that is currently available. If `start` itself is unavailable, the return
    /// value is `start - 1`.
    ///
    /// "Available" is context dependent:
    /// - On the publisher side it means consumers have freed the slot.
    /// - On the consumer side it means publishers have published the slot.
    fn scan_available_until(&self, start: Sequence, end: Sequence) -> Sequence;
}

pub(crate) struct U32SlotStates {
    states: Box<[AtomicU32]>,
    index_shift: i64,
    ring_meta: RingBufferMeta,
    flag_adjustment: u32,
}

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

    pub(crate) fn new_all_available(ring_meta: RingBufferMeta) -> Self {
        let capacity = ring_meta.buffer_size() as usize;
        let states = (0..capacity).map(|_| AtomicU32::new(u32::MAX)).collect();

        let index_shift = capacity.trailing_zeros() as i64;
        Self {
            states,
            index_shift,
            ring_meta,
            flag_adjustment: 0,
        }
    }

    fn slot_index(&self, seq: Sequence) -> usize {
        self.ring_meta.index_of_seq(seq)
    }

    fn available_flag(&self, seq: Sequence) -> u32 {
        ((seq.value() >> self.index_shift) as u32).wrapping_add(self.flag_adjustment)
    }

    fn slot_state(&self, seq: Sequence) -> (usize, u32) {
        let index = self.slot_index(seq);
        let flag = self.available_flag(seq);
        (index, flag)
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
    fn scan_available_until(&self, start: Sequence, end: Sequence) -> Sequence {
        debug_assert!(start <= end, "scan range must be ordered");
        let mut last_available = start.value() - 1;
        for seq_value in start.value()..=end.value() {
            let seq = Sequence::new(seq_value);
            let (index, flag) = self.slot_state(seq);
            if self.states[index].load(Ordering::Relaxed) != flag {
                break;
            }
            last_available = seq_value;
        }
        // Ensure readers observe slot writes for all sequences <= last_available.
        if last_available >= start.value() {
            fence(Ordering::Acquire);
        }
        Sequence::new(last_available)
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

        // Empty scans should return start - 1.
        assert_eq!(
            states.scan_available_until(Sequence::new(0), Sequence::new(0)),
            Sequence::new(-1)
        );
        assert_eq!(
            states.scan_available_until(Sequence::new(5), Sequence::new(7)),
            Sequence::new(4)
        );

        // Publishing a single sequence should make it available.
        states.publish(Sequence::new(0));
        assert_eq!(
            states.scan_available_until(Sequence::new(0), Sequence::new(0)),
            Sequence::new(0)
        );

        // Publishing with a hole should stop at the last contiguous sequence.
        states.publish(Sequence::new(2));
        assert_eq!(
            states.scan_available_until(Sequence::new(0), Sequence::new(2)),
            Sequence::new(0)
        );

        // Filling the hole makes the range contiguous.
        states.publish(Sequence::new(1));
        assert_eq!(
            states.scan_available_until(Sequence::new(0), Sequence::new(2)),
            Sequence::new(2)
        );

        // publish_range should make the entire inclusive range available.
        states.publish_range(Sequence::new(3), Sequence::new(5));
        assert_eq!(
            states.scan_available_until(Sequence::new(0), Sequence::new(5)),
            Sequence::new(5)
        );

        // Test scanning a subset of available range
        assert_eq!(
            states.scan_available_until(Sequence::new(3), Sequence::new(4)),
            Sequence::new(4)
        );
    }

    fn run_lap_handling_suite<G: SlotStateGroup>(make: impl Fn(RingBufferMeta) -> G) {
        let ring = RingBufferMeta::new(4); // Small ring for easier wrapping
        let states = make(ring);

        // Publish seq 0 (lap 0, slot 0).
        states.publish(Sequence::new(0));
        assert_eq!(
            states.scan_available_until(Sequence::new(0), Sequence::new(0)),
            Sequence::new(0)
        );

        // Publish seq 4 (lap 1, same slot 0). This should overwrite slot 0.
        states.publish(Sequence::new(4));

        // Seq 0 should no longer be available.
        assert_eq!(
            states.scan_available_until(Sequence::new(0), Sequence::new(0)),
            Sequence::new(-1)
        );
        // Seq 4 should be available.
        assert_eq!(
            states.scan_available_until(Sequence::new(4), Sequence::new(4)),
            Sequence::new(4)
        );

        // Test publish_range causing wrap-around overwrite
        // Current state: Slot 0 has seq 4. Slots 1,2,3 are empty (seq -1 effectively).

        // Publish range 2..5.
        // Slot 2 -> seq 2 (lap 0)
        // Slot 3 -> seq 3 (lap 0)
        // Slot 0 -> seq 4 (lap 1) - already there, but publish_range should handle it idempotent or update
        // Slot 1 -> seq 5 (lap 1)
        states.publish_range(Sequence::new(2), Sequence::new(5));

        // Check availability
        // Seq 2..3 should be available
        assert_eq!(
            states.scan_available_until(Sequence::new(2), Sequence::new(3)),
            Sequence::new(3)
        );

        // Seq 4..5 should be available
        assert_eq!(
            states.scan_available_until(Sequence::new(4), Sequence::new(5)),
            Sequence::new(5)
        );

        // Seq 0..1 should NOT be available (overwritten by 4, 5)
        assert_eq!(
            states.scan_available_until(Sequence::new(0), Sequence::new(1)),
            Sequence::new(-1)
        );

        // Mixed scan: 2..5.
        // Slot 2 (seq 2) -> OK
        // Slot 3 (seq 3) -> OK
        // Slot 0 (seq 4) -> OK
        // Slot 1 (seq 5) -> OK
        assert_eq!(
            states.scan_available_until(Sequence::new(2), Sequence::new(5)),
            Sequence::new(5)
        );
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

    generate_slot_state_tests!(U32SlotStates, "u32_new_all_available", |ring| {
        U32SlotStates::new_all_available(ring)
    });
}
