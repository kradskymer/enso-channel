use std::sync::Arc;

use crate::permit::SendBatch;
use crate::{
    RingBuffer, Sequence,
    errors::{TrySendAtMostError, TrySendError},
    sequencers::Sequencer,
};

#[derive(Clone)]
pub struct Publisher<P, T> {
    publisher_sequencer: P,
    ring_buffer: Arc<RingBuffer<T>>,
}

impl<P: Sequencer, T> Publisher<P, T> {
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

    #[inline]
    pub(crate) fn write_at_value(&self, seq: i64, item: T) {
        self.write(item, Sequence::new(seq));
    }

    #[inline]
    pub(crate) fn commit_range_value(&self, start_seq: i64, end_seq: i64) {
        self.publisher_sequencer
            .commit_range(Sequence::new(start_seq), Sequence::new(end_seq));
    }

    pub fn try_publish(&mut self, item: T) -> Result<(), TrySendError> {
        let seq = self.publisher_sequencer.try_claim()?;
        self.write(item, seq);
        self.publisher_sequencer.commit(seq);
        Ok(())
    }

    pub fn try_publish_many<F>(
        &mut self,
        n: usize,
        factory: F,
    ) -> Result<SendBatch<'_, P, T, F>, TrySendError>
    where
        F: Fn() -> T + Copy,
    {
        assert!(n > 0, "n must be > 0");
        let (start_seq, end_seq) = self.publisher_sequencer.try_claim_n(n as i64)?;

        Ok(SendBatch::new(
            &*self,
            start_seq.value(),
            end_seq.value(),
            factory,
        ))
    }

    pub fn try_publish_many_default(
        &mut self,
        n: usize,
    ) -> Result<SendBatch<'_, P, T, fn() -> T>, TrySendError>
    where
        T: Default,
    {
        self.try_publish_many(n, T::default)
    }

    pub fn try_publish_at_most<F>(
        &mut self,
        limit: usize,
        factory: F,
    ) -> Result<SendBatch<'_, P, T, F>, TrySendAtMostError>
    where
        F: Fn() -> T + Copy,
    {
        assert!(limit > 0, "limit must be > 0");

        let (start_seq, end_seq) = self.publisher_sequencer.try_claim_at_most(limit as i64)?;

        Ok(SendBatch::new(
            &*self,
            start_seq.value(),
            end_seq.value(),
            factory,
        ))
    }

    pub fn try_publish_at_most_default(
        &mut self,
        limit: usize,
    ) -> Result<SendBatch<'_, P, T, fn() -> T>, TrySendAtMostError>
    where
        T: Default,
    {
        self.try_publish_at_most(limit, T::default)
    }
}
