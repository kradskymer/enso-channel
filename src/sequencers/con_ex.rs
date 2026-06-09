use std::sync::{atomic::Ordering, Arc};

use crate::sequencers::{ConsumerBarrier, ConsumerNotify, SlotState};
use crate::waker::Waker;
use crate::{Cursor, Sequence};

#[derive(Clone)]
pub(crate) struct ExclusiveConSeqGate {
    pub(crate) consumed: Arc<Cursor>,
    pub(crate) waker: Arc<Waker>,
}

impl ExclusiveConSeqGate {
    pub(crate) fn new() -> Self {
        let waker = Arc::new(Waker::new());
        let consumed = Arc::new(crate::Cursor::new(crate::Sequence::INIT));
        Self { consumed, waker }
    }
}

impl ConsumerNotify for ExclusiveConSeqGate {
    fn notify(&self) {
        self.waker.wake();
    }
}

impl ConsumerBarrier for ExclusiveConSeqGate {
    fn max_consumed(&self) -> SlotState {
        let value = self.consumed.load(Ordering::Acquire);
        let seq = Sequence::new(value);
        SlotState::new(seq, seq.is_shutdown_open())
    }
}
