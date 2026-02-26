use crossbeam_utils::CachePadded;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::Sequence;

pub(crate) struct Cursor(CachePadded<AtomicI64>);

impl Cursor {
    #[allow(clippy::declare_interior_mutable_const)]
    pub const INIT: Self = Self(CachePadded::new(AtomicI64::new(Sequence::INIT.value())));

    #[inline]
    pub(crate) fn new(value: Sequence) -> Self {
        Self(CachePadded::new(AtomicI64::new(value.value())))
    }

    #[inline]
    pub(crate) fn store(&self, value: i64, order: Ordering) {
        self.0.store(value, order);
    }

    #[inline]
    pub(crate) fn load(&self, order: Ordering) -> i64 {
        self.0.load(order)
    }

    #[inline]
    pub(crate) fn compare_exchange(
        &self,
        current: i64,
        new: i64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<i64, i64> {
        // Use a *strong* CAS to avoid spurious failures.
        //
        // Several call sites (e.g. shutdown/close paths) rely on a single attempt
        // to transition a sentinel value; using `compare_exchange_weak` there can
        // spuriously fail and leave the channel in an "open" state.
        self.0.compare_exchange(current, new, success, failure)
    }
}
