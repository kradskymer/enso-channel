use enso_channel::errors::{TryRecvError, TrySendError};

use super::shared::Channel;

pub trait MpscChannel: Channel {
    fn try_send_batch_write_exact(
        sender: &mut Self::Sender,
        items: &[u32],
    ) -> Result<(), TrySendError>;
}

pub trait CloneSender: MpscChannel {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender;
}

pub fn send_and_recv<C: MpscChannel>() {
    let (mut tx, mut rx) = C::channel(8);

    C::try_send(&mut tx, 10).unwrap();
    C::try_send(&mut tx, 11).unwrap();

    assert_eq!(C::try_recv(&mut rx).unwrap(), 10);
    assert_eq!(C::try_recv(&mut rx).unwrap(), 11);
}

pub fn recv_empty_returns_insufficient_items<C: MpscChannel>() {
    let (_tx, mut rx) = C::channel(8);

    match C::try_recv(&mut rx) {
        Err(TryRecvError::InsufficientItems { .. }) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(v) => panic!("expected error, got Ok({v})"),
    }
}

pub fn send_batch_write_exact<C: MpscChannel>() {
    let (mut tx, mut rx) = C::channel(8);

    C::try_send_batch_write_exact(&mut tx, &[1, 2]).unwrap();

    assert_eq!(C::try_recv(&mut rx).unwrap(), 1);
    assert_eq!(C::try_recv(&mut rx).unwrap(), 2);
}

pub fn sender_is_cloneable<C: CloneSender>() {
    let (mut tx1, mut rx) = C::channel(8);
    let mut tx2 = C::clone_sender(&tx1);

    C::try_send(&mut tx1, 1).unwrap();
    C::try_send(&mut tx2, 2).unwrap();

    assert_eq!(C::try_recv(&mut rx).unwrap(), 1);
    assert_eq!(C::try_recv(&mut rx).unwrap(), 2);
}
