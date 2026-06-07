use std::sync::Arc;

use crate::errors::{TryReserveError, TrySendAtMostError};
use crate::guards::{WritePermitImpl, WritePermitsImpl};
use crate::{errors::TrySendError, sequencers::Sequencer, RingBuffer, Sequence};
use crate::{ChanWritePermit, ChanWritePermits, SlotRecycler};

#[derive(Clone)]
pub(crate) struct SenderImpl<P, T> {
    publisher_sequencer: P,
    ring_buffer: Arc<RingBuffer<T>>,
}

impl<P: Sequencer, T> SenderImpl<P, T> {
    pub(crate) fn new(publisher_sequencer: P, ring_buffer: Arc<RingBuffer<T>>) -> Self {
        Self {
            publisher_sequencer,
            ring_buffer,
        }
    }

    #[inline]
    fn write(&self, item: T, seq: Sequence) {
        self.ring_buffer.modify_at(seq, |slot| {
            *slot = item;
        });
    }
}

impl<P: Sequencer, T> ChannelSender<T> for SenderImpl<P, T> {
    type WritePermit<'this, S>
        = WritePermitImpl<'this, T, S, P>
    where
        Self: 'this,
        S: SlotRecycler<T>;

    type WritePermits<'this, S>
        = WritePermitsImpl<'this, T, S, P>
    where
        Self: 'this,
        S: SlotRecycler<T>;

    fn try_send(&mut self, value: T) -> Result<(), TrySendError<T>> {
        match self.publisher_sequencer.try_claim() {
            Ok(seq) => {
                self.write(value, seq);
                self.publisher_sequencer.commit(seq);
                Ok(())
            }
            Err(crate::errors::TryClaimError::Empty) => Err(TrySendError::Full(value)),
            Err(crate::errors::TryClaimError::Shutdown) => Err(TrySendError::Disconnected(value)),
        }
    }

    fn try_reserve<S: SlotRecycler<T>>(
        &mut self,
        recycler: S,
    ) -> Result<Self::WritePermit<'_, S>, TryReserveError> {
        let seq = self.publisher_sequencer.try_claim()?;
        Ok(WritePermitImpl {
            sequence: seq,
            publisher_sequencer: &self.publisher_sequencer,
            ring_buffer: &self.ring_buffer,
            wrote: false,
            recycler,
        })
    }

    fn try_send_at_most<S: SlotRecycler<T>>(
        &mut self,
        limit: usize,
        recycler: S,
    ) -> Result<Self::WritePermits<'_, S>, TrySendAtMostError> {
        if limit == 0 {
            return Ok(WritePermitsImpl {
                ring_buffer: &self.ring_buffer,
                publisher_sequencer: &mut self.publisher_sequencer,
                start_seq: 0,
                end_seq: 0,
                next_seq: 0,
                recycler,
            });
        }
        let (start_seq, end_seq) = self.publisher_sequencer.try_claim_at_most(limit as i64)?;
        Ok(WritePermitsImpl {
            ring_buffer: &self.ring_buffer,
            publisher_sequencer: &mut self.publisher_sequencer,
            start_seq: start_seq.value(),
            end_seq: end_seq.value(),
            next_seq: start_seq.value(),
            recycler,
        })
    }
}

/// A sender for a channel that allows sending values of type `T`.
pub trait ChannelSender<T> {
    /// A write permit of a reserved slot in the channel.
    type WritePermit<'this, S>: ChanWritePermit<T>
    where
        Self: 'this,
        S: SlotRecycler<T>;

    /// A batch of write permits of consecutive slots in the channel.
    type WritePermits<'this, S>: ChanWritePermits<T>
    where
        Self: 'this,
        S: SlotRecycler<T>;

    /// Tries to send a value of type `T` to the channel.
    ///
    /// Returns `Ok(())` if the value was sent successfully
    /// or an error if the channel is full or disconnected.
    fn try_send(&mut self, value: T) -> Result<(), TrySendError<T>>;

    /// Tries to reserve a slot in the channel for a value of type `T`.
    ///
    /// Returns a write permit if the slot was reserved successfully without channel closed.
    /// or an error if the channel is full or disconnected.
    fn try_reserve<S: SlotRecycler<T>>(
        &mut self,
        recycler: S,
    ) -> Result<Self::WritePermit<'_, S>, TryReserveError>;

    /// Tries to send up to `limit` values of type `T` to the channel.
    ///
    /// Returns a [`ChanWritePermits`] if slots were claimed (the batch may contain
    /// fewer than `limit` slots if fewer are available) or an error if the channel
    /// is full or disconnected.
    ///
    /// If `limit` is `0`, this method will return an empty [`ChanWritePermits`] immediately
    /// without checking the channel state.
    fn try_send_at_most<S: SlotRecycler<T>>(
        &mut self,
        limit: usize,
        recycler: S,
    ) -> Result<Self::WritePermits<'_, S>, TrySendAtMostError>;
}
