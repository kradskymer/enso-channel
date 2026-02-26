use enso_channel::errors::{TryRecvError, TrySendError};

use super::shared::Channel;

pub trait QueueChannel: Channel {}

pub trait CloneSender: QueueChannel {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender;
}

pub trait CloneReceiver: QueueChannel {
    fn clone_receiver(receiver: &Self::Receiver) -> Self::Receiver;
}

pub fn basic_send_recv<C: QueueChannel>() {
    let (mut tx, mut rx) = C::channel(8);
    assert!(C::try_send(&mut tx, 1).is_ok());
    assert_eq!(C::try_recv(&mut rx).ok(), Some(1));
}

pub fn item_delivered_to_exactly_one_of_two_consumers<C: QueueChannel + CloneReceiver>() {
    let (mut tx, rx1) = C::channel(8);
    let rx2 = C::clone_receiver(&rx1);

    let mut rx1 = rx1;
    let mut rx2 = rx2;

    assert!(C::try_send(&mut tx, 7).is_ok());

    let a = C::try_recv(&mut rx1).ok();
    let b = C::try_recv(&mut rx2).ok();

    assert!(
        a.is_some() ^ b.is_some(),
        "exactly one receiver should get the item"
    );
}

/// Test that work-stealing consumers can split work across clones.
pub fn consumers_split_work<C: QueueChannel + CloneReceiver>() {
    let (mut tx, rx1) = C::channel(8);
    let mut rx2 = C::clone_receiver(&rx1);
    let mut rx1 = rx1;

    // Send two items
    C::try_send(&mut tx, 1).expect("send 1");
    C::try_send(&mut tx, 2).expect("send 2");

    // Each consumer should get one item
    let a = C::try_recv(&mut rx1).ok();
    let b = C::try_recv(&mut rx2).ok();

    assert!(
        a.is_some() && b.is_some(),
        "each consumer should get an item"
    );
    assert_ne!(a, b, "consumers should get different items");
}

/// Test dropping the last producer disconnects consumers.
pub fn producer_drop_disconnects_consumers<C: QueueChannel>() {
    let (mut tx, mut rx) = C::channel(8);

    C::try_send(&mut tx, 1).expect("send should succeed");
    drop(tx);

    // Should get the item first
    assert_eq!(C::try_recv(&mut rx).ok(), Some(1));

    // Then disconnected
    match C::try_recv(&mut rx) {
        Err(TryRecvError::Disconnected) => {}
        other => panic!("expected Disconnected, got {other:?}"),
    }
}

/// Test dropping the last consumer disconnects producers.
pub fn consumer_drop_disconnects_producers<C: QueueChannel>() {
    let (mut tx, rx) = C::channel(8);

    drop(rx);

    match C::try_send(&mut tx, 1) {
        Err(TrySendError::Disconnected) => {}
        other => panic!("expected Disconnected, got {other:?}"),
    }
}
