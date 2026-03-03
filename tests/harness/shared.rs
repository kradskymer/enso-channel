//! Shared trait extensions for channel test harnesses.
//!
//! These traits allow generic contract tests to be written once and applied
//! to all channel variants that implement the required capabilities.

use enso_channel::errors::{TryRecvAtMostError, TryRecvError, TrySendAtMostError, TrySendError};

/// Core channel trait that all test harnesses must implement.
pub trait Channel {
    type Sender;
    type Receiver;

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

    // Try to receive exactly n items.
    fn try_recv_many(receiver: &mut Self::Receiver, items: usize)
        -> Result<Vec<u32>, TryRecvError>;

    // Try to receive at most n items.
    fn try_recv_at_most(
        receiver: &mut Self::Receiver,
        max_items: usize,
    ) -> Result<Vec<u32>, TryRecvAtMostError>;
}
