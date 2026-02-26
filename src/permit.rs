use crate::publisher::Publisher;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("exact length mismatch: expected {expected}, got {got}")]
pub struct ExactLenMismatch {
    pub expected: usize,
    pub got: usize,
}

/// A guard representing an already-claimed contiguous sequence range.
pub(crate) struct SendBatch<'a, P, T, F>
where
    P: crate::sequencers::Sequencer,
    F: Fn() -> T + Copy,
{
    publisher: &'a Publisher<P, T>,
    start_seq: i64,
    end_seq: i64,
    next_seq: i64,
    factory: F,
    committed: bool,
}

impl<P, T, F> std::fmt::Debug for SendBatch<'_, P, T, F>
where
    P: crate::sequencers::Sequencer,
    F: Fn() -> T + Copy,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendBatch")
            .field("start_seq", &self.start_seq)
            .field("end_seq", &self.end_seq)
            .field("next_seq", &self.next_seq)
            .field("committed", &self.committed)
            .finish()
    }
}

impl<'a, P, T, F> SendBatch<'a, P, T, F>
where
    P: crate::sequencers::Sequencer,
    F: Fn() -> T + Copy,
{
    pub(crate) fn new(
        publisher: &'a Publisher<P, T>,
        start_seq: i64,
        end_seq: i64,
        factory: F,
    ) -> Self {
        debug_assert!(start_seq <= end_seq, "invalid send batch range");
        Self {
            publisher,
            start_seq,
            end_seq,
            next_seq: start_seq,
            factory,
            committed: false,
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        (self.end_seq - self.start_seq + 1) as usize
    }

    #[inline]
    pub fn remaining(&self) -> usize {
        if self.next_seq > self.end_seq {
            0
        } else {
            (self.end_seq - self.next_seq + 1) as usize
        }
    }

    #[inline]
    pub fn try_write_next(&mut self, item: T) -> Result<(), T> {
        if self.next_seq > self.end_seq {
            return Err(item);
        }

        self.publisher.write_at_value(self.next_seq, item);
        self.next_seq += 1;
        Ok(())
    }

    #[inline]
    pub fn write_next(&mut self, item: T) {
        if self.try_write_next(item).is_err() {
            panic!("SendBatch::write_next called after batch is full");
        }
    }

    #[inline]
    pub fn write_from_iter<I>(&mut self, iter: I) -> usize
    where
        I: IntoIterator<Item = T>,
    {
        let mut written = 0;
        for item in iter {
            if self.try_write_next(item).is_err() {
                break;
            }
            written += 1;
        }
        written
    }

    #[inline]
    pub fn try_write_exact<I>(&mut self, iter: I) -> Result<(), ExactLenMismatch>
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: ExactSizeIterator,
    {
        let it = iter.into_iter();
        let got = it.len();
        let expected = self.remaining();
        if got != expected {
            return Err(ExactLenMismatch { expected, got });
        }

        for item in it {
            // We just verified `got == remaining`, so this cannot become full mid-loop.
            debug_assert!(self.next_seq <= self.end_seq);
            self.publisher.write_at_value(self.next_seq, item);
            self.next_seq += 1;
        }

        Ok(())
    }

    #[inline]
    pub fn write_exact<I>(&mut self, iter: I)
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: ExactSizeIterator,
    {
        if let Err(err) = self.try_write_exact(iter) {
            panic!(
                "SendBatch::write_exact iterator length mismatch: expected {}, got {}",
                err.expected, err.got
            );
        }
    }

    #[inline]
    pub fn fill_with<G>(&mut self, mut make: G)
    where
        G: FnMut() -> T,
    {
        while self.next_seq <= self.end_seq {
            self.publisher.write_at_value(self.next_seq, make());
            self.next_seq += 1;
        }
    }

    #[inline]
    pub fn finish(mut self) {
        self.fill_remaining_with_factory();
        self.commit();
    }

    #[inline]
    fn fill_remaining_with_factory(&mut self) {
        while self.next_seq <= self.end_seq {
            self.publisher
                .write_at_value(self.next_seq, (self.factory)());
            self.next_seq += 1;
        }
    }

    #[inline]
    fn commit(&mut self) {
        if self.committed {
            return;
        }
        self.committed = true;
        self.publisher
            .commit_range_value(self.start_seq, self.end_seq);
    }
}

impl<P, T, F> Drop for SendBatch<'_, P, T, F>
where
    P: crate::sequencers::Sequencer,
    F: Fn() -> T + Copy,
{
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        self.fill_remaining_with_factory();
        self.commit();
    }
}
