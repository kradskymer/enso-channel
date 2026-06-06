use std::sync::Arc;

use crate::errors::{TryReserveError, TrySendAtMostError};
use crate::guards::{WritePermitImpl, WritePermitsImpl};
use crate::{errors::TrySendError, sequencers::Sequencer, RingBuffer, Sequence};
use crate::{ChanWritePermit, ChanWritePermits};

#[derive(Clone)]
pub(crate) struct SenderImpl<P, T> {
    publisher_sequencer: P,
    ring_buffer: Arc<RingBuffer<T>>,
}

impl<P: Sequencer, T: Sentinel> SenderImpl<P, T> {
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

impl<P: Sequencer, T: Sentinel> ChannelSender<T> for SenderImpl<P, T> {
    type WritePermit<'this>
        = WritePermitImpl<'this, T, P>
    where
        Self: 'this;

    type WritePermits<'this>
        = WritePermitsImpl<'this, T, P>
    where
        Self: 'this;

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

    fn try_reserve(&mut self) -> Result<Self::WritePermit<'_>, TryReserveError> {
        let seq = self.publisher_sequencer.try_claim()?;
        Ok(WritePermitImpl {
            sequence: seq,
            publisher_sequencer: &self.publisher_sequencer,
            ring_buffer: &self.ring_buffer,
            wrote: false,
        })
    }

    fn try_send_at_most(
        &mut self,
        limit: usize,
    ) -> Result<Self::WritePermits<'_>, TrySendAtMostError> {
        if limit == 0 {
            return Ok(WritePermitsImpl {
                ring_buffer: &self.ring_buffer,
                publisher_sequencer: &mut self.publisher_sequencer,
                start_seq: 0,
                end_seq: 0,
                next_seq: 0,
            });
        }
        let (start_seq, end_seq) = self.publisher_sequencer.try_claim_at_most(limit as i64)?;
        Ok(WritePermitsImpl {
            ring_buffer: &self.ring_buffer,
            publisher_sequencer: &mut self.publisher_sequencer,
            start_seq: start_seq.value(),
            end_seq: end_seq.value(),
            next_seq: start_seq.value(),
        })
    }
}

/// A trait for types that can be used as sentinel values in the ring buffer.
///
/// A sentinel value is used as the dummy value in the ring buffer to indicate
/// that a slot is empty.
pub trait Sentinel: Sized {
    fn sentinel() -> Self;
}

impl<T: Default> Sentinel for T {
    fn sentinel() -> Self {
        T::default()
    }
}

/// A sender for a channel that allows sending values of type `T`.
pub trait ChannelSender<T> {
    /// A write permit of a reserved slot in the channel.
    type WritePermit<'this>: ChanWritePermit<T>
    where
        Self: 'this;

    /// A batch of write permits of consecutive slots in the channel.
    type WritePermits<'this>: ChanWritePermits<T>
    where
        Self: 'this;

    /// Tries to send a value of type `T` to the channel.
    ///
    /// Returns `Ok(())` if the value was sent successfully
    /// or an error if the channel is full or disconnected.
    fn try_send(&mut self, value: T) -> Result<(), TrySendError<T>>;

    /// Tries to reserve a slot in the channel for a value of type `T`.
    ///
    /// Returns a write permit if the slot was reserved successfully without channel closed.
    /// or an error if the channel is full or disconnected.
    fn try_reserve(&mut self) -> Result<Self::WritePermit<'_>, TryReserveError>;

    /// Tries to send up to `limit` values of type `T` to the channel.
    ///
    /// Returns a [`ChanWritePermits`] if slots were claimed (the batch may contain
    /// fewer than `limit` slots if fewer are available) or an error if the channel
    /// is full or disconnected.
    ///
    /// If `limit` is `0`, this method will return an empty [`ChanWritePermits`] immediately
    /// without checking the channel state.
    fn try_send_at_most(
        &mut self,
        limit: usize,
    ) -> Result<Self::WritePermits<'_>, TrySendAtMostError>;
}
