use std::sync::{atomic::Ordering, Arc};

use crate::sequencers::{ConsumerBarrier, SlotState};
use crate::{Cursor, Sequence};

#[derive(Clone)]
pub(crate) struct ExclusiveConSeqGate {
    pub(crate) consumed: Arc<Cursor>,
    #[cfg(feature = "async-receiver")]
    pub(crate) waker: Arc<crate::waker::Waker>,
}

impl ExclusiveConSeqGate {
    #[cfg(feature = "async-receiver")]
    pub(crate) fn new() -> Self {
        let waker = Arc::new(crate::waker::Waker::new());
        let consumed = Arc::new(crate::Cursor::new(crate::Sequence::INIT));
        Self { consumed, waker }
    }

    #[cfg(not(feature = "async-receiver"))]
    pub(crate) fn new() -> Self {
        let consumed = Arc::new(crate::Cursor::new(crate::Sequence::INIT));
        Self { consumed }
    }
}

impl ConsumerBarrier for ExclusiveConSeqGate {
    fn max_consumed(&self) -> SlotState {
        let value = self.consumed.load(Ordering::Acquire);
        let seq = Sequence::new(value);
        SlotState::new(seq, seq.is_shutdown_open())
    }

    #[cfg(feature = "async-receiver")]
    fn notify(&self) {
        self.waker.wake();
    }
}
