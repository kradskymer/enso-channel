use std::sync::{atomic::Ordering, Arc};

use crate::{
    sequencers::{ConsumerBarrier, SlotState},
    Cursor, Sequence,
};

#[derive(Clone)]
pub(crate) struct FanoutConSeqGate<const N: usize> {
    pub(crate) consumed: [Arc<Cursor>; N],
    #[cfg(feature = "async-receiver")]
    pub(crate) wakers: [Arc<crate::waker::Waker>; N],
}

impl<const N: usize> FanoutConSeqGate<N> {
    #[cfg(feature = "async-receiver")]
    pub(crate) fn new() -> Self {
        let wakers = std::array::from_fn(|_| Arc::new(crate::waker::Waker::new()));
        let consumed: [Arc<crate::Cursor>; N] =
            std::array::from_fn(|_| Arc::new(crate::Cursor::new(crate::Sequence::INIT)));
        Self { consumed, wakers }
    }

    #[cfg(not(feature = "async-receiver"))]
    pub(crate) fn new() -> Self {
        let consumed: [Arc<crate::Cursor>; N] =
            std::array::from_fn(|_| Arc::new(crate::Cursor::new(crate::Sequence::INIT)));
        Self { consumed }
    }
}

// impl<const N: usize> ConsumerNotify for FanoutConSeqGate<N> {}

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

    #[cfg(feature = "async-receiver")]
    fn notify(&self) {
        for i in 0..N {
            self.wakers[i].wake();
        }
    }
}
