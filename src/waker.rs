use atomic_waker::AtomicWaker;
use crossbeam_utils::CachePadded;

pub struct Waker(CachePadded<AtomicWaker>);

impl Waker {
    pub fn new() -> Self {
        Self(CachePadded::new(AtomicWaker::new()))
    }

    pub fn wake(&self) {
        self.0.wake();
    }

    pub fn register(&self, waker: &std::task::Waker) {
        self.0.register(waker);
    }
}
