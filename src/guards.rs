// ---------------------------------------------------------------------------
// Single-slot Permit (from try_reserve)
// ---------------------------------------------------------------------------

use std::sync::Arc;

use crate::{sequencers::Sequencer, RingBuffer, Sentinel, Sequence};

pub(crate) struct WritePermitImpl<'a, T, P>
where
    P: Sequencer,
    T: Sentinel,
{
    pub sequence: Sequence,
    pub publisher_sequencer: &'a P,
    pub ring_buffer: &'a Arc<RingBuffer<T>>,
    pub wrote: bool,
}

impl<T, P> ChanWritePermit<T> for WritePermitImpl<'_, T, P>
where
    P: Sequencer,
    T: Sentinel,
{
    fn write(mut self, item: T) {
        self.ring_buffer.modify_at(self.sequence, |slot| {
            *slot = item;
        });
        self.wrote = true;
    }
}

impl<T, P> Drop for WritePermitImpl<'_, T, P>
where
    P: Sequencer,
    T: Sentinel,
{
    fn drop(&mut self) {
        if !self.wrote {
            self.ring_buffer.modify_at(self.sequence, |slot| {
                *slot = T::sentinel();
            });
        }
        self.publisher_sequencer.commit(self.sequence);
    }
}

// ---------------------------------------------------------------------------
// Multiple contiguous batch permits
// ---------------------------------------------------------------------------

pub(crate) struct BatchWritePermitImpl<'batch, 'a, T, P>
where
    P: Sequencer,
    T: Sentinel,
{
    sequence: Sequence,
    batch: &'batch mut WritePermitsImpl<'a, T, P>,
    wrote: bool,
}

impl<T, P> ChanWritePermit<T> for BatchWritePermitImpl<'_, '_, T, P>
where
    P: Sequencer,
    T: Sentinel,
{
    fn write(mut self, item: T) {
        self.batch.write(self.sequence, item);
        self.wrote = true;
    }
}

impl<T, P> Drop for BatchWritePermitImpl<'_, '_, T, P>
where
    P: Sequencer,
    T: Sentinel,
{
    fn drop(&mut self) {
        if !self.wrote {
            self.batch.write(self.sequence, T::sentinel());
        }
    }
}

pub(crate) struct WritePermitsImpl<'a, T, P>
where
    P: Sequencer,
    T: Sentinel,
{
    pub ring_buffer: &'a RingBuffer<T>,
    pub publisher_sequencer: &'a mut P,
    pub start_seq: i64,
    pub end_seq: i64,
    pub next_seq: i64,
}

impl<'a, T, P> WritePermitsImpl<'a, T, P>
where
    P: Sequencer,
    T: Sentinel,
{
    fn next(&mut self) -> Option<BatchWritePermitImpl<'_, 'a, T, P>> {
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
}

impl<'a, T, P> Drop for WritePermitsImpl<'a, T, P>
where
    T: Sentinel,
    P: Sequencer,
{
    fn drop(&mut self) {
        while self.next_seq <= self.end_seq {
            self.write(Sequence::new(self.next_seq), T::sentinel());
            self.next_seq += 1;
        }
        self.publisher_sequencer
            .commit_range(Sequence::new(self.start_seq), Sequence::new(self.end_seq));
    }
}

impl<'a, T, P> ChanWritePermits<T> for WritePermitsImpl<'a, T, P>
where
    T: Sentinel,
    P: Sequencer,
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
    fn iter(&'a self) -> impl Iterator<Item = &'a T> + 'a;

    /// Consumes the reference, releasing it to the channel.
    fn finish(self);
}
