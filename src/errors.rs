//! Error types returned by non-blocking `try_*` channel operations.
//!
//! All channel topologies in this crate expose explicit backpressure by returning
//! typed errors instead of blocking.

/// Internal error type for Sequencer operations.
///
/// This is not exposed in the public API. It gets converted to appropriate
/// public error types (TrySendError, TryRecvError, etc.) at the Publisher/Consumer layer.
#[derive(thiserror::Error, Debug)]
pub(crate) enum TryClaimError {
    #[error("Insufficient capacity: missing {missing} sequences")]
    Insufficient { missing: i64 },

    #[error("The channel is empty")]
    Empty,

    #[error("The channel is shutdown")]
    Shutdown,
}

#[derive(thiserror::Error, Debug)]
/// Error returned by `try_send*` operations that require an exact claim size.
pub enum TrySendError {
    /// The ring buffer does not currently have enough free capacity.
    #[error("Insufficient capacity: missing {missing} sequences")]
    InsufficientCapacity { missing: usize },

    /// The channel was disconnected (e.g. all receivers were dropped).
    #[error("The channel is disconnected")]
    Disconnected,
}

#[derive(thiserror::Error, Debug)]
/// Error returned by `try_recv*` operations that require an exact claim size.
pub enum TryRecvError {
    /// Not enough items are currently available.
    #[error("Insufficient items: missing {missing} sequences")]
    InsufficientItems { missing: usize },

    /// The channel was disconnected (e.g. all senders were dropped).
    #[error("The channel is disconnected")]
    Disconnected,
}

#[derive(thiserror::Error, Debug)]
/// Error returned by `try_send_at_most*` operations.
pub enum TrySendAtMostError {
    /// No capacity is currently available (0 slots could be claimed).
    #[error("The channel is full")]
    Full,

    /// The channel was disconnected (e.g. all receivers were dropped).
    #[error("The channel is disconnected")]
    Disconnected,
}

#[derive(thiserror::Error, Debug)]
/// Error returned by `try_recv_at_most` operations.
pub enum TryRecvAtMostError {
    /// No items are currently available (0 items could be claimed).
    #[error("The channel is empty")]
    Empty,

    /// The channel was disconnected (e.g. all senders were dropped).
    #[error("The channel is disconnected")]
    Disconnected,
}

impl From<TryClaimError> for TrySendError {
    #[inline]
    fn from(err: TryClaimError) -> Self {
        match err {
            TryClaimError::Insufficient { missing } => TrySendError::InsufficientCapacity {
                missing: missing as usize,
            },
            TryClaimError::Empty => {
                unreachable!("Publisher sequencers should not return Empty")
            }
            TryClaimError::Shutdown => TrySendError::Disconnected,
        }
    }
}

impl From<TryClaimError> for TryRecvError {
    #[inline]
    fn from(err: TryClaimError) -> Self {
        match err {
            TryClaimError::Insufficient { missing } => TryRecvError::InsufficientItems {
                missing: missing as usize,
            },
            TryClaimError::Empty => {
                unreachable!("Empty should be handled before conversion in Consumer")
            }
            TryClaimError::Shutdown => TryRecvError::Disconnected,
        }
    }
}

impl From<TryClaimError> for TrySendAtMostError {
    #[inline]
    fn from(err: TryClaimError) -> Self {
        match err {
            TryClaimError::Empty => TrySendAtMostError::Full,
            TryClaimError::Shutdown => TrySendAtMostError::Disconnected,
            TryClaimError::Insufficient { .. } => {
                unreachable!("Insufficient should be handled before conversion in Publisher")
            }
        }
    }
}

impl From<TryClaimError> for TryRecvAtMostError {
    #[inline]
    fn from(err: TryClaimError) -> Self {
        match err {
            TryClaimError::Empty => TryRecvAtMostError::Empty,
            TryClaimError::Shutdown => TryRecvAtMostError::Disconnected,
            TryClaimError::Insufficient { .. } => {
                unreachable!("Insufficient should be handled before conversion in Consumer")
            }
        }
    }
}
