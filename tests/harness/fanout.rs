use enso_channel::errors::{TryRecvError, TrySendError};

use super::shared::Channel;

/// Trait for fanout channels with a fixed number of receivers.
pub trait FanoutChannel<const N: usize>: Channel {
    fn channel_with_receivers(capacity: usize) -> (Self::Sender, [Self::Receiver; N]);
}

/// Adapter to test fanout channels with the shared `Channel` trait.
/// Uses only the first receiver for contract tests.
pub trait FanoutAdapter<const N: usize> {
    type Sender;
    type Receiver;

    fn channel_fanout(capacity: usize) -> (Self::Sender, [Self::Receiver; N]);
    fn try_send(sender: &mut Self::Sender, item: u32) -> Result<(), TrySendError>;
    fn try_recv(receiver: &mut Self::Receiver) -> Result<u32, TryRecvError>;
}

pub fn each_receiver_sees_all_items<const N: usize, C: FanoutAdapter<N>>() {
    let (mut tx, mut rxs) = C::channel_fanout(8);

    assert!(C::try_send(&mut tx, 1).is_ok());
    assert!(C::try_send(&mut tx, 2).is_ok());

    for rx in rxs.iter_mut() {
        assert_eq!(C::try_recv(rx).ok(), Some(1));
        assert_eq!(C::try_recv(rx).ok(), Some(2));
    }
}

pub fn publisher_is_backpressured_by_slowest<C: FanoutAdapter<2>>() {
    let (mut tx, mut rxs) = C::channel_fanout(2);

    // Fill the buffer.
    assert!(C::try_send(&mut tx, 1).is_ok());
    assert!(C::try_send(&mut tx, 2).is_ok());

    // Fast consumer drains, slow consumer does not.
    assert_eq!(C::try_recv(&mut rxs[0]).ok(), Some(1));
    assert_eq!(C::try_recv(&mut rxs[0]).ok(), Some(2));

    // Still blocked, because the slowest consumer hasn't advanced at all.
    assert!(C::try_send(&mut tx, 3).is_err());

    // Once the slow consumer advances one slot, publisher can make progress.
    assert_eq!(C::try_recv(&mut rxs[1]).ok(), Some(1));
    assert!(C::try_send(&mut tx, 3).is_ok());
}

/// Test that dropping the last publisher eventually disconnects all consumers.
pub fn publisher_drop_disconnects_consumers<C, const N: usize>()
where
    C: FanoutAdapter<N>,
{
    let (mut tx, mut rxs) = C::channel_fanout(8);

    C::try_send(&mut tx, 1).expect("send should succeed");
    drop(tx);

    // Each receiver should get the item, then Disconnected.
    for rx in rxs.iter_mut() {
        assert_eq!(C::try_recv(rx).ok(), Some(1));
        match C::try_recv(rx) {
            Err(TryRecvError::Disconnected) => {}
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }
}
