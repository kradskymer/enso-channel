use std::cell::UnsafeCell;

use crate::Sequence;

pub(crate) struct RingBuffer<T> {
    slots: Box<[UnsafeCell<T>]>,
    ring_meta: RingBufferMeta,
}

// SAFETY: RingBuffer can be sent between threads when T is Send.
// The slots are only accessed through the sequencer/gate protocol which ensures:
// - Single-writer per sequence: claim mechanism guarantees only one publisher writes each slot
// - No concurrent access: publisher commits (Release fence) before consumer reads (Acquire fence)
#[allow(unsafe_code)]
unsafe impl<T: Send> Send for RingBuffer<T> {}

// SAFETY: RingBuffer can be shared between threads when T is Send.
// Concurrent access to the same slot never occurs due to the sequencer protocol:
// 1. Publisher claims sequence s → writes to slot → Release fence → publishes flag
// 2. Consumer observes flag → Acquire fence → reads from slot → commits consumption
// 3. Only after consumer commits can publisher reclaim the slot
//
// The fence-based publication scheme (see slot_states.rs) establishes happens-before:
// - Publisher: normal writes → Release fence → relaxed flag store
// - Consumer: relaxed flag load → Acquire fence → reads
//
// This ensures that all writes by the publisher are visible to the consumer when
// the consumer observes the published flag.
#[allow(unsafe_code)]
unsafe impl<T: Send> Sync for RingBuffer<T> {}

impl<T> RingBuffer<T> {
    pub fn init_with_default(capacity: usize) -> Self
    where
        T: Default,
    {
        Self::init_with(capacity, |_| T::default())
    }

    pub fn init_with<F>(capacity: usize, initializer: F) -> Self
    where
        F: Copy + FnOnce(usize) -> T,
    {
        let ring_meta = RingBufferMeta::new(capacity);
        let slots = (0..capacity)
            .map(|i| UnsafeCell::new(initializer(i)))
            .collect();
        Self { slots, ring_meta }
    }
    #[inline]
    pub const fn capacity(&self) -> i64 {
        self.ring_meta.buffer_size()
    }

    #[allow(unsafe_code)]
    #[inline]
    pub(crate) fn modify_at<F>(&self, seq: Sequence, modifier: F)
    where
        F: FnOnce(&mut T),
    {
        let index = self.ring_meta.index_of_seq(seq);
        // SAFETY: index is always in the valid range by a masking operation
        unsafe { modifier(&mut *self.slots.get_unchecked(index).get()) }
    }

    #[allow(unsafe_code)]
    #[inline]
    pub(crate) fn get_ref_at(&self, seq: Sequence) -> &T {
        let index = self.ring_meta.index_of_seq(seq);
        // SAFETY: index is always in the valid range by a masking operation
        unsafe { &*self.slots.get_unchecked(index).get() }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RingBufferMeta {
    index_mask: i64,
    buffer_size: i64,
}

impl RingBufferMeta {
    pub(crate) fn new(buffer_size: usize) -> Self {
        assert!(
            buffer_size.is_power_of_two(),
            "buffer_size must be a power of two"
        );
        assert!(buffer_size <= i64::MAX as usize, "buffer_size too large");

        let index_mask = buffer_size as i64 - 1;
        let buffer_size = buffer_size as i64;
        Self {
            index_mask,
            buffer_size,
        }
    }

    #[inline]
    pub const fn index_of_seq(&self, seq: Sequence) -> usize {
        (seq.value() & self.index_mask) as usize
    }

    #[inline]
    pub const fn buffer_size(&self) -> i64 {
        self.buffer_size
    }

    #[inline]
    pub fn available_slots(&self, highest_published: Sequence, max_consumed: Sequence) -> i64 {
        let wrap_point = highest_published - self.buffer_size;
        (max_consumed - wrap_point).into()
    }
}

#[cfg(test)]
mod tests {
    use super::RingBuffer;
    use super::RingBufferMeta;
    use crate::Sequence;

    #[test]
    fn test_invalid_capacity_should_panic() {
        let result = std::panic::catch_unwind(|| {
            RingBuffer::<u32>::init_with_default(7);
        });
        assert!(result.is_err());
    }

    #[test]
    fn test_ringbuffer_capacity() {
        let rb: RingBuffer<u32> = RingBuffer::init_with_default(8);
        assert_eq!(rb.capacity(), 8);
    }

    #[test]
    fn test_index_of_seq() {
        let meta = RingBufferMeta::new(8);
        assert_eq!(meta.index_of_seq(Sequence::new(0)), 0);
        assert_eq!(meta.index_of_seq(Sequence::new(1)), 1);
        assert_eq!(meta.index_of_seq(Sequence::new(7)), 7);
        assert_eq!(meta.index_of_seq(Sequence::new(8)), 0);
        assert_eq!(meta.index_of_seq(Sequence::new(9)), 1);
        assert_eq!(meta.index_of_seq(Sequence::new(15)), 7);
        assert_eq!(meta.index_of_seq(Sequence::new(16)), 0);
    }

    #[test]
    fn test_available_slots() {
        // none consumed or published
        let meta = RingBufferMeta::new(4);
        assert_eq!(meta.available_slots(Sequence::INIT, Sequence::INIT), 4);

        // first round
        assert_eq!(meta.available_slots(Sequence::new(0), Sequence::INIT), 3);
        assert_eq!(meta.available_slots(Sequence::new(1), Sequence::INIT), 2);
        assert_eq!(meta.available_slots(Sequence::new(2), Sequence::INIT), 1);
        assert_eq!(meta.available_slots(Sequence::new(3), Sequence::new(0)), 1);
        assert_eq!(meta.available_slots(Sequence::new(3), Sequence::new(3)), 4);

        // second round
        assert_eq!(meta.available_slots(Sequence::new(4), Sequence::new(3)), 3);
        assert_eq!(meta.available_slots(Sequence::new(5), Sequence::new(3)), 2);
        assert_eq!(meta.available_slots(Sequence::new(6), Sequence::new(3)), 1);
        assert_eq!(meta.available_slots(Sequence::new(7), Sequence::new(4)), 1);
        assert_eq!(meta.available_slots(Sequence::new(8), Sequence::new(4)), 0);
    }
}
