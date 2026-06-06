use enso_channel::{ChanReadRefs, ChanReceiver, ChanWritePermit, ChanWritePermits, ChannelSender};

fn main() {
    let (mut tx, mut rx) = enso_channel::mpsc::channel(16).unwrap();

    let mut tx_cp = tx.clone();
    tx.try_send(1).unwrap();
    tx_cp.try_send(2).unwrap();

    let mut batch_permits = tx.try_send_at_most(10).unwrap();
    for i in 3..12 {
        if let Some(permit) = batch_permits.next() {
            permit.write(i);
        }
    }
    batch_permits.commit();

    let batch = rx.try_recv_at_most(30).unwrap();
    for (i, expected) in batch.iter().zip(1..12) {
        assert_eq!(*i, expected);
    }
}
