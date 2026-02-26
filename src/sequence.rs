#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Sequence(i64);

impl Sequence {
    pub const INIT: Self = Self(-1);
    pub(crate) const SHUTDOWN_OPEN: Self = Self(i64::MIN);

    #[inline]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    #[inline]
    pub const fn value(self) -> i64 {
        self.0
    }

    #[inline]
    pub(crate) const fn is_shutdown_open(self) -> bool {
        self.0 == Self::SHUTDOWN_OPEN.0
    }

    #[inline]
    pub const fn is_init(self) -> bool {
        self.0 == Self::INIT.0
    }
}

impl core::ops::Add<i64> for Sequence {
    type Output = Sequence;

    fn add(self, rhs: i64) -> Sequence {
        Sequence(self.0 + rhs)
    }
}

impl core::ops::Sub<i64> for Sequence {
    type Output = Sequence;

    fn sub(self, rhs: i64) -> Sequence {
        Sequence(self.0 - rhs)
    }
}

impl core::ops::AddAssign<i64> for Sequence {
    fn add_assign(&mut self, rhs: i64) {
        self.0 += rhs;
    }
}

impl core::cmp::PartialEq<i64> for Sequence {
    fn eq(&self, other: &i64) -> bool {
        self.0 == *other
    }
}

impl core::cmp::PartialOrd<i64> for Sequence {
    fn partial_cmp(&self, other: &i64) -> Option<core::cmp::Ordering> {
        self.0.partial_cmp(other)
    }
}

impl From<i64> for Sequence {
    fn from(value: i64) -> Self {
        Sequence::new(value)
    }
}

impl From<Sequence> for i64 {
    fn from(seq: Sequence) -> Self {
        seq.value()
    }
}

impl core::ops::Sub<Sequence> for Sequence {
    type Output = Sequence;

    fn sub(self, rhs: Sequence) -> Sequence {
        Sequence(self.0 - rhs.0)
    }
}

impl std::fmt::Display for Sequence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::Sequence;

    #[test]
    fn adds_and_subs_with_i64() {
        let seq = Sequence::new(5);
        assert_eq!(seq + 3, Sequence::new(8));
        assert_eq!(seq - 2, Sequence::new(3));
    }

    #[test]
    fn compares_to_i64() {
        let seq = Sequence::new(7);
        assert!(seq == 7);
        assert!(seq > 5);
        assert!(seq < 10);
    }
}
