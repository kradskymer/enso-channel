use std::ops::Deref;
use std::sync::Arc;

use crate::errors::TryRecvError;
use crate::sequencers::{ConsumerSequencer, Sequencer};
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
    C: ConsumerSequencer,
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
            return Ok(empty_read_refs(&self.consumer_sequencer, &self.ring_buffer));
        }
        let (start_seq, end_seq) = self.consumer_sequencer.try_claim_at_most(limit as i64)?;
        Ok(ReadRefsImpl {
            consumer_sequencer: &self.consumer_sequencer,
            ring_buffer: &self.ring_buffer,
            start_seq,
            end_seq,
        })
    }

    async fn recv_async(&mut self) -> Option<Self::ReadRef<'_>> {
        use std::future::poll_fn;
        let poll = poll_fn(|cx| self.consumer_sequencer.claim_at_most_async(1, cx)).await;
        match poll {
            Ok((start_seq, end_seq)) => Some(ReadRefImpl {
                consumer_sequencer: &self.consumer_sequencer,
                seq: start_seq,
                slot: self.ring_buffer.get_ref_at(end_seq),
            }),
            Err(_) => None,
        }
    }

    async fn recv_at_most_async<'a>(&'a mut self, limit: usize) -> Option<Self::ReadRefs<'a>>
    where
        T: 'a,
    {
        if limit == 0 {
            return Some(empty_read_refs(&self.consumer_sequencer, &self.ring_buffer));
        }
        use std::future::poll_fn;
        let poll = poll_fn(|cx| {
            self.consumer_sequencer
                .claim_at_most_async(limit as i64, cx)
        })
        .await;
        match poll {
            Ok((start_seq, end_seq)) => Some(ReadRefsImpl {
                consumer_sequencer: &self.consumer_sequencer,
                ring_buffer: &self.ring_buffer,
                start_seq,
                end_seq,
            }),
            Err(_) => None,
        }
    }
}

fn empty_read_refs<'a, T, C>(
    consumer_sequencer: &'a C,
    ring_buffer: &'a RingBuffer<T>,
) -> ReadRefsImpl<'a, T, C>
where
    C: Sequencer,
{
    ReadRefsImpl {
        consumer_sequencer,
        ring_buffer,
        start_seq: Sequence::new(0),
        end_seq: Sequence::INIT,
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

impl<C, T> Drop for ReadRefsImpl<'_, T, C>
where
    C: Sequencer,
{
    fn drop(&mut self) {
        if self.end_seq == Sequence::INIT {
            return;
        }
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
    fn iter(&self) -> impl Iterator<Item = &'a T> + '_ {
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

    /// Receives a single value from the channel asynchronously.
    ///
    /// Returns a [`ChanReadRef`] if a value was available, or `None` if the channel is disconnected.
    fn recv_async(&mut self) -> impl std::future::Future<Output = Option<Self::ReadRef<'_>>>;

    /// Receives a batch of values from the channel asynchronously.
    ///
    /// Returns a [`ChanReadRefs`] if slots were claimed (the batch may contain fewer values than `limit`
    /// slots if fewer are available), or `None` if the channel is disconnected.
    ///
    /// If `limit` is `0`, this method will return an empty [`ChanReadRefs`] immediately without
    /// checking the channel's state.
    fn recv_at_most_async<'a>(
        &'a mut self,
        limit: usize,
    ) -> impl std::future::Future<Output = Option<Self::ReadRefs<'a>>>
    where
        T: 'a;
}
