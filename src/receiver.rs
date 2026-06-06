use std::ops::Deref;
use std::sync::Arc;

use crate::errors::TryRecvError;
use crate::sequencers::Sequencer;
use crate::{ChanReadRef, ChanReadRefs, RingBuffer, Sequence};

pub(crate) struct ReceiverImpl<C, T> {
    consumer_sequencer: C,
    ring_buffer: Arc<RingBuffer<T>>,
}

impl<C, T> ReceiverImpl<C, T> {
    pub fn new(consumer_sequencer: C, ring_buffer: Arc<RingBuffer<T>>) -> Self {
        Self {
            ring_buffer,
            consumer_sequencer,
        }
    }
}

impl<C, T> ChanReceiver<T> for ReceiverImpl<C, T>
where
    C: Sequencer,
{
    type ReadRef<'this>
        = ReadRefImpl<'this, T, C>
    where
        Self: 'this;

    type ReadRefs<'this>
        = ReadRefsImpl<'this, T, C>
    where
        Self: 'this,
        T: 'this;

    fn try_recv(&mut self) -> Result<ReadRefImpl<'_, T, C>, TryRecvError> {
        let seq = self.consumer_sequencer.try_claim()?;
        let slot = self.ring_buffer.get_ref_at(seq);
        Ok(ReadRefImpl {
            consumer_sequencer: &self.consumer_sequencer,
            seq,
            slot,
        })
    }

    fn try_recv_at_most(&mut self, limit: usize) -> Result<ReadRefsImpl<'_, T, C>, TryRecvError> {
        if limit == 0 {
            return Ok(ReadRefsImpl::new(
                &self.consumer_sequencer,
                &self.ring_buffer,
                Sequence::new(0),
                Sequence::new(0),
            ));
        }
        let (start_seq, end_seq) = self.consumer_sequencer.try_claim_at_most(limit as i64)?;
        Ok(ReadRefsImpl::new(
            &self.consumer_sequencer,
            &self.ring_buffer,
            start_seq,
            end_seq,
        ))
    }
}

pub(crate) struct ReadRefImpl<'a, T, C>
where
    C: Sequencer,
{
    pub(super) consumer_sequencer: &'a C,
    pub(super) seq: Sequence,
    pub(super) slot: &'a T,
}

impl<'a, C, T> Deref for ReadRefImpl<'a, T, C>
where
    C: Sequencer,
{
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.slot
    }
}

impl<'a, C, T> Drop for ReadRefImpl<'a, T, C>
where
    C: Sequencer,
{
    fn drop(&mut self) {
        self.consumer_sequencer.commit(self.seq);
    }
}

pub(crate) struct ReadRefsImpl<'a, T, C>
where
    C: Sequencer,
{
    consumer_sequencer: &'a C,
    ring_buffer: &'a RingBuffer<T>,
    start_seq: Sequence,
    end_seq: Sequence,
}

impl<'a, C, T> ReadRefsImpl<'a, T, C>
where
    C: Sequencer,
{
    fn new(
        consumer_sequencer: &'a C,
        ring_buffer: &'a RingBuffer<T>,
        start_seq: Sequence,
        end_seq: Sequence,
    ) -> Self {
        Self {
            consumer_sequencer,
            ring_buffer,
            start_seq,
            end_seq,
        }
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
        ReadBatchIter {
            ring_buffer: self.ring_buffer,
            current_seq: self.start_seq,
            end_seq: self.end_seq,
        }
    }

    #[inline]
    pub fn finish(self) {
        drop(self);
    }
}

impl<C, T> Drop for ReadRefsImpl<'_, T, C>
where
    C: Sequencer,
{
    fn drop(&mut self) {
        self.consumer_sequencer
            .commit_range(self.start_seq, self.end_seq);
    }
}

pub(crate) struct ReadBatchIter<'a, T> {
    ring_buffer: &'a RingBuffer<T>,
    current_seq: Sequence,
    end_seq: Sequence,
}

impl<'a, T> Iterator for ReadBatchIter<'a, T> {
    type Item = &'a T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.current_seq > self.end_seq {
            return None;
        }
        let slot = self.ring_buffer.get_ref_at(self.current_seq);
        self.current_seq += 1;
        Some(slot)
    }
}

impl<'a, C, T> ChanReadRef<'a, T> for ReadRefImpl<'a, T, C>
where
    C: Sequencer,
{
    fn finish(self) {
        drop(self)
    }
}

impl<'a, C, T> ChanReadRefs<'a, T> for ReadRefsImpl<'a, T, C>
where
    C: Sequencer,
{
    fn iter(&self) -> impl Iterator<Item = &'a T> + 'a {
        ReadBatchIter {
            ring_buffer: self.ring_buffer,
            current_seq: self.start_seq,
            end_seq: self.end_seq,
        }
    }

    fn finish(self) {
        drop(self)
    }
}

/// A trait for receiving values from a channel.
pub trait ChanReceiver<T> {
    /// A readable reference to a value of a specific slot from the channel.
    type ReadRef<'this>: ChanReadRef<'this, T>
    where
        Self: 'this;

    /// A readable reference to a batch of values of consecutive slots from the channel.
    type ReadRefs<'this>: ChanReadRefs<'this, T>
    where
        Self: 'this,
        T: 'this;

    /// Tries to receive a single value from the channel.
    ///
    /// Returns a [`ChanReadRef`] if a value was available, or an error if the channel
    /// is empty or disconnected.
    fn try_recv(&mut self) -> Result<Self::ReadRef<'_>, TryRecvError>;

    /// Tries to receive a batch of values from the channel.
    ///
    /// Returns a [`ChanReadRefs`] if slots were claimed (the batch may contain fewer values than `limit`
    /// slots if fewer are available), or an error if the channel is empty or disconnected.
    ///
    /// If `limit` is `0`, this method will return an empty [`ChanReadRefs`] immediately without
    /// checking the channel's state.
    fn try_recv_at_most(&mut self, limit: usize) -> Result<Self::ReadRefs<'_>, TryRecvError>;
}
