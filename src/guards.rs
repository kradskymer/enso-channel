// ---------------------------------------------------------------------------
// Single-slot Permit (from try_reserve)
// ---------------------------------------------------------------------------

use std::{panic::AssertUnwindSafe, sync::Arc};

use crate::{sequencers::ProducerSequencer, RingBuffer, Sequence, SlotRecycler};

pub(crate) struct WritePermitImpl<'a, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    pub sequence: Sequence,
    pub publisher_sequencer: &'a P,
    pub ring_buffer: &'a Arc<RingBuffer<T>>,
    pub recycler: S,
    pub wrote: bool,
}

impl<T, S, P> ChanWritePermit<T> for WritePermitImpl<'_, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn write(mut self, item: T) {
        self.ring_buffer.modify_at(self.sequence, |slot| {
            *slot = item;
        });
        self.wrote = true;
    }

    fn update_in_place(mut self, f: impl FnOnce(&mut T)) {
        self.ring_buffer.modify_at(self.sequence, |slot| {
            f(slot);
        });
        self.wrote = true;
    }
}

impl<T, S, P> Drop for WritePermitImpl<'_, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn drop(&mut self) {
        if !self.wrote
            && std::panic::catch_unwind(AssertUnwindSafe(|| {
                self.ring_buffer.modify_at(self.sequence, |slot| {
                    self.recycler.recycle(slot);
                });
            }))
            .is_err()
        {
            // The recycler panicked — terminate the channel.
            self.publisher_sequencer.terminate();
            return;
        }
        self.publisher_sequencer.commit(self.sequence);
    }
}

impl<T, S, P> std::fmt::Debug for WritePermitImpl<'_, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WritePermitImpl")
            .field("sequence", &self.sequence)
            .field("wrote", &self.wrote)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Multiple contiguous batch permits
// ---------------------------------------------------------------------------

pub(crate) struct BatchWritePermitImpl<'batch, 'a, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    sequence: Sequence,
    batch: &'batch mut WritePermitsImpl<'a, T, S, P>,
    wrote: bool,
}

impl<T, S, P> ChanWritePermit<T> for BatchWritePermitImpl<'_, '_, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn write(mut self, item: T) {
        self.batch.write(self.sequence, item);
        self.wrote = true;
    }

    fn update_in_place(mut self, f: impl FnOnce(&mut T)) {
        self.batch.update_in_place(self.sequence, f);
        self.wrote = true;
    }
}

impl<T, S, P> Drop for BatchWritePermitImpl<'_, '_, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn drop(&mut self) {
        if !self.wrote
            && std::panic::catch_unwind(AssertUnwindSafe(|| self.batch.recycle(self.sequence)))
                .is_err()
        {
            self.batch.publisher_sequencer.terminate();
        }
    }
}

pub(crate) struct WritePermitsImpl<'a, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    pub ring_buffer: &'a RingBuffer<T>,
    pub publisher_sequencer: &'a P,
    pub start_seq: i64,
    pub end_seq: i64,
    pub next_seq: i64,
    pub recycler: S,
}

impl<'a, T, S, P> WritePermitsImpl<'a, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn next(&mut self) -> Option<BatchWritePermitImpl<'_, 'a, T, S, P>> {
        if self.next_seq > self.end_seq {
            return None;
        }
        let seq = self.next_seq;
        self.next_seq += 1;

        Some(BatchWritePermitImpl {
            sequence: Sequence::new(seq),
            batch: self,
            wrote: false,
        })
    }

    fn write(&self, seq: Sequence, value: T) {
        self.ring_buffer.modify_at(seq, |slot| *slot = value);
    }

    fn update_in_place(&self, seq: Sequence, f: impl FnOnce(&mut T)) {
        self.ring_buffer.modify_at(seq, |slot| f(slot));
    }

    fn recycle(&self, seq: Sequence) {
        self.ring_buffer
            .modify_at(seq, |slot| self.recycler.recycle(slot));
    }
}

impl<'a, T, S, P> Drop for WritePermitsImpl<'a, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn drop(&mut self) {
        if self.end_seq == -1 {
            return;
        }
        while self.next_seq <= self.end_seq {
            let seq = Sequence::new(self.next_seq);
            let ring = self.ring_buffer;
            let recycler_ref = &self.recycler;
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                ring.modify_at(seq, |slot| recycler_ref.recycle(slot));
            }));
            if result.is_err() {
                // The recycler panicked — terminate the channel.
                self.publisher_sequencer.terminate();
                return;
            }
            self.next_seq += 1;
        }
        self.publisher_sequencer
            .commit_range(Sequence::new(self.start_seq), Sequence::new(self.end_seq));
    }
}

impl<'a, T, S, P> std::fmt::Debug for WritePermitsImpl<'a, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WritePermitsImpl")
            .field("start_seq", &self.start_seq)
            .field("end_seq", &self.end_seq)
            .field("next_seq", &self.next_seq)
            .finish()
    }
}

impl<'a, T, S, P> ChanWritePermits<T> for WritePermitsImpl<'a, T, S, P>
where
    P: ProducerSequencer,
    S: SlotRecycler<T>,
{
    fn total_reserved(&self) -> usize {
        (self.end_seq - self.start_seq + 1) as usize
    }

    fn next(&mut self) -> Option<impl ChanWritePermit<T>> {
        self.next()
    }

    fn commit(self) {
        drop(self)
    }
}

/// Indicate a reserved slot in the channel for writing a value.
///
/// # Drop
///
/// Dropping a [`ChanWritePermit`] will write the sentinel value to the channel,
/// if the value has not been written yet.
/// Dropping a [`ChanWritePermit`] will commit the written value to the channel,
/// after which consumer(s) can read the value.
pub trait ChanWritePermit<T> {
    /// Write the value to the reserved slot.
    /// This method will consume the permit, so it cannot be used again.
    fn write(self, value: T);

    fn update_in_place(self, f: impl FnOnce(&mut T));
}

/// Indicates a batch of consecutive reserved slots in the channel for writing values.
///
/// # Drop
///
/// Dropping a [`ChanWritePermits`] will write the sentinel value to the channel,
/// for any values that have not been written yet.
/// Dropping a [`ChanWritePermits`] will commit the written values to the channel,
/// after which consumer(s) can read the values.
pub trait ChanWritePermits<T> {
    /// The number of total reserved slots in the channel for writing values.
    fn total_reserved(&self) -> usize;

    /// Returns the next permit for writing a value, if one is available.
    fn next(&mut self) -> Option<impl ChanWritePermit<T>>;

    /// Commit the batch of permits so that the receiver(s) can read the values.
    fn commit(self);
}

/// A readable reference to a value from the channel.
///
/// # Drop
///
/// Dropping a [`ChanReadRef`] will release the reference to the channel,
/// allowing the writer to write a new value to the slot.
///
/// If the reference is dropped without the value being read, that value is lost
/// to this consumer (the slot is released back to the channel).
pub trait ChanReadRef<'a, T>: std::ops::Deref<Target = T> {
    /// Consumes the reference, releasing it to the channel.
    fn finish(self);
}

/// A readable reference to one or more consecutive values from the channel.
///
/// # Drop
///
/// Dropping a [`ChanReadRefs`] will release the reference to the channel,
/// allowing the writer to write a new value to all the slots in the batch.
///
/// If the reference is dropped without the values being read, those values are
/// lost to this consumer (the slots are released back to the channel).
pub trait ChanReadRefs<'a, T: 'a> {
    /// Returns an iterator over the values in the batch.
    fn iter(&self) -> impl Iterator<Item = &'a T> + '_;

    /// Consumes the reference, releasing it to the channel.
    fn finish(self);
}
