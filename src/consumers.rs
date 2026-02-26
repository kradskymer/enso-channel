use std::ops::Deref;
use std::sync::Arc;

use crate::errors::{TryRecvAtMostError, TryRecvError};
use crate::sequencers::Sequencer;
use crate::{RingBuffer, Sequence};

pub struct Consumer<C, T> {
    consumer_sequencer: C,
    ring_buffer: Arc<RingBuffer<T>>,
}

impl<C: Clone, T> Clone for Consumer<C, T> {
    fn clone(&self) -> Self {
        Self {
            consumer_sequencer: self.consumer_sequencer.clone(),
            ring_buffer: self.ring_buffer.clone(),
        }
    }
}

impl<C, T> Consumer<C, T> {
    pub fn new(consumer_sequencer: C, ring_buffer: Arc<RingBuffer<T>>) -> Self {
        Self {
            ring_buffer,
            consumer_sequencer,
        }
    }
}

impl<C, T> Consumer<C, T>
where
    C: Sequencer,
{
    pub fn try_recv(&mut self) -> Result<ReadGuard<'_, C, T>, TryRecvError> {
        let seq = self.consumer_sequencer.try_claim()?;
        let slot = self.ring_buffer.get_ref_at(seq);
        Ok(ReadGuard {
            consumer_sequencer: &self.consumer_sequencer,
            seq,
            slot,
        })
    }

    pub fn try_recv_many(&mut self, n: i64) -> Result<ReadIter<'_, C, T>, TryRecvError> {
        assert!(n > 0, "n must be positive");

        let (start_seq, end_seq) = self.consumer_sequencer.try_claim_n(n)?;
        Ok(ReadIter::new(
            &self.consumer_sequencer,
            &self.ring_buffer,
            start_seq,
            end_seq,
        ))
    }

    pub fn try_recv_at_most(
        &mut self,
        limit: i64,
    ) -> Result<ReadIter<'_, C, T>, TryRecvAtMostError> {
        assert!(limit > 0, "limit must be > 0");

        let (start_seq, end_seq) = self.consumer_sequencer.try_claim_at_most(limit)?;
        Ok(ReadIter::new(
            &self.consumer_sequencer,
            &self.ring_buffer,
            start_seq,
            end_seq,
        ))
    }
}

pub struct ReadGuard<'a, C, T>
where
    C: Sequencer,
{
    pub(super) consumer_sequencer: &'a C,
    pub(super) seq: Sequence,
    pub(super) slot: &'a T,
}

impl<'a, C, T> Deref for ReadGuard<'a, C, T>
where
    C: Sequencer,
{
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        self.slot
    }
}

impl<'a, C, T> Drop for ReadGuard<'a, C, T>
where
    C: Sequencer,
{
    fn drop(&mut self) {
        self.consumer_sequencer.commit(self.seq);
    }
}

pub struct ReadIter<'a, C, T>
where
    C: Sequencer,
{
    pub(super) consumer_sequencer: &'a C,
    pub(super) ring_buffer: &'a RingBuffer<T>,
    pub(super) current_seq: Sequence,
    pub(super) start_seq: Sequence,
    pub(super) end_seq: Sequence,
}

impl<'a, C, T> ReadIter<'a, C, T>
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
            current_seq: start_seq,
            start_seq,
            end_seq,
        }
    }
}

impl<'a, C, T> Iterator for ReadIter<'a, C, T>
where
    C: Sequencer,
{
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

impl<'a, C, T> Drop for ReadIter<'a, C, T>
where
    C: Sequencer,
{
    fn drop(&mut self) {
        self.consumer_sequencer
            .commit_range(self.start_seq, self.end_seq);
    }
}
