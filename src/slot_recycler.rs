/// Defines how a stale slot should be updated on the sender side.
pub trait SlotRecycler<T> {
    fn recycle(&self, stale_value: &mut T);
}

impl<F, T> SlotRecycler<T> for F
where
    F: Fn(&mut T),
{
    #[inline(always)]
    fn recycle(&self, stale_value: &mut T) {
        self(stale_value);
    }
}

/// Resets a stale value using a function that produces a sentinel value.
#[derive(Clone, Copy)]
pub struct ResetWith<F>(pub F);

impl<T, F> SlotRecycler<T> for ResetWith<F>
where
    F: Fn() -> T,
{
    #[inline(always)]
    fn recycle(&self, stale_value: &mut T) {
        *stale_value = (self.0)();
    }
}

#[derive(Clone, Copy)]
pub struct ResetWithDefault;

impl<T: Default> SlotRecycler<T> for ResetWithDefault {
    #[inline(always)]
    fn recycle(&self, stale_value: &mut T) {
        *stale_value = T::default();
    }
}

/// Resets a stale value using a cloneable sentinel value.
#[derive(Clone, Copy)]
pub struct ResetWithCloneable<T>(pub T);

impl<T> SlotRecycler<T> for ResetWithCloneable<T>
where
    T: Clone,
{
    #[inline(always)]
    fn recycle(&self, stale_value: &mut T) {
        *stale_value = self.0.clone();
    }
}

#[cfg(test)]
mod tests {
    use crate::slot_recycler::{ResetWith, SlotRecycler};

    #[test]
    fn reset_with_default() {
        let mut value: i32 = 42;
        let reset = ResetWith(i32::default);
        reset.recycle(&mut value);
        assert_eq!(value, 0);
    }

    #[test]
    fn test_closure_recycle() {
        fn accept_closure_recycle<S: SlotRecycler<i32>>(s: S) {
            let mut value: i32 = 42;
            s.recycle(&mut value);
            assert_eq!(value, 0);
        }
        accept_closure_recycle(|i: &mut _| *i = 0);
    }
}
