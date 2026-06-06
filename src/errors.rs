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
    #[error("The channel is empty")]
    Empty,

    #[error("The channel is shutdown")]
    Shutdown,
}

#[derive(thiserror::Error, Debug)]
pub enum TrySendError<T> {
    Full(T),
    Disconnected(T),
}

#[derive(thiserror::Error, Debug)]
pub enum TryReserveError {
    #[error("The channel is full")]
    Full,
    #[error("The consumers are disconnected")]
    Disconnected,
}

#[derive(thiserror::Error, Debug)]
pub enum TrySendAtMostError {
    #[error("The channel is full")]
    Full,
    #[error("The consumers are disconnected")]
    Disconnected,
}

#[derive(thiserror::Error, Debug)]
/// Error returned by `try_recv*` operations.
pub enum TryRecvError {
    // /// Not enough items are currently available.
    #[error("The channel is empty")]
    Empty,

    /// The channel was disconnected (e.g. all senders were dropped).
    #[error("The channel is disconnected")]
    Disconnected,
}

impl From<TryClaimError> for TryRecvError {
    #[inline]
    fn from(err: TryClaimError) -> Self {
        match err {
            TryClaimError::Empty => TryRecvError::Empty,
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
        }
    }
}

impl From<TryClaimError> for TryReserveError {
    #[inline]
    fn from(err: TryClaimError) -> Self {
        match err {
            TryClaimError::Empty => TryReserveError::Full,
            TryClaimError::Shutdown => TryReserveError::Disconnected,
        }
    }
}
