use std::sync::{atomic::Ordering, Arc};

use crate::{
    sequencers::{ConsumerBarrier, ConsumerNotify, SlotState},
    waker::Waker,
    Cursor, Sequence,
};

#[derive(Clone)]
pub(crate) struct FanoutConSeqGate<const N: usize> {
    pub(crate) consumed: [Arc<Cursor>; N],
    pub(crate) wakers: [Arc<Waker>; N],
}

impl<const N: usize> FanoutConSeqGate<N> {
    pub(crate) fn new() -> Self {
        let wakers = std::array::from_fn(|_| Arc::new(Waker::new()));
        let consumed: [Arc<crate::Cursor>; N] =
            std::array::from_fn(|_| Arc::new(crate::Cursor::new(crate::Sequence::INIT)));
        Self { consumed, wakers }
    }
}

impl<const N: usize> ConsumerNotify for FanoutConSeqGate<N> {
    fn notify(&self) {
        for i in 0..N {
            self.wakers[i].wake();
        }
    }
}

impl<const N: usize> ConsumerBarrier for FanoutConSeqGate<N> {
    fn max_consumed(&self) -> SlotState {
        let mut min_consumed = Sequence::SHUTDOWN_OPEN;
        for i in 0..N {
            let value = self.consumed[i].load(Ordering::Acquire);
            let seq = Sequence::new(value);
            min_consumed = min_consumed.min(seq);
        }
        SlotState::new(min_consumed, min_consumed.is_shutdown_open())
    }
}
