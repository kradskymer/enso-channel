//! Shared trait extensions for channel test harnesses.
//!
//! These traits allow generic contract tests to be written once and applied
//! to all channel variants that implement the required capabilities.

use enso_channel::errors::{TryRecvAtMostError, TryRecvError, TrySendAtMostError, TrySendError};

/// Core channel trait that all test harnesses must implement.
pub trait Channel {
    type Sender;
    type Receiver;

    /// The batch guard type returned by `try_recv_many_batch` / `try_recv_at_most_batch`.
    ///
    /// This is the *primitive* receive-batch API for the harness.
    type RecvBatch<'a>: RecvBatchU32
    where
        Self::Receiver: 'a;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver);

    fn try_send(sender: &mut Self::Sender, item: u32) -> Result<(), TrySendError>;

    // Try to send n items from the provided slice and n == items.len()
    fn try_send_many(sender: &mut Self::Sender, items: &[u32]) -> Result<(), TrySendError>;

    // Try to send at most n items from the provided slice.
    fn try_send_at_most(
        sender: &mut Self::Sender,
        items: &[u32],
    ) -> Result<usize, TrySendAtMostError>;

    fn try_recv(receiver: &mut Self::Receiver) -> Result<u32, TryRecvError>;

    /// Try to receive exactly `n` items, returning a batch guard on success.
    fn try_recv_many_batch<'a>(
        receiver: &'a mut Self::Receiver,
        n: usize,
    ) -> Result<Self::RecvBatch<'a>, TryRecvError>;

    /// Try to receive up to `limit` items, returning a batch guard on success.
    fn try_recv_at_most_batch<'a>(
        receiver: &'a mut Self::Receiver,
        limit: usize,
    ) -> Result<Self::RecvBatch<'a>, TryRecvAtMostError>;

    /// Convenience wrapper: receive exactly `n` items and collect via `.iter()`.
    fn try_recv_many(receiver: &mut Self::Receiver, n: usize) -> Result<Vec<u32>, TryRecvError> {
        Ok(Self::try_recv_many_batch(receiver, n)?.to_vec())
    }

    /// Convenience wrapper: receive up to `limit` items and collect via `.iter()`.
    fn try_recv_at_most(
        receiver: &mut Self::Receiver,
        limit: usize,
    ) -> Result<Vec<u32>, TryRecvAtMostError> {
        Ok(Self::try_recv_at_most_batch(receiver, limit)?.to_vec())
    }
}

/// Test-only helper for working with the public `try_recv_*` batch guard types.
///
/// The production API intentionally does not expose raw sequence boundaries; in tests we only
/// need to (a) iterate via `.iter()` and (b) explicitly commit via `.finish()`.
pub trait RecvBatchU32 {
    /// Collect all items from the batch via the safe `.iter()` API.
    fn to_vec(&self) -> Vec<u32>;

    /// Commit the batch immediately (equivalent to dropping it).
    fn finish(self);
}
