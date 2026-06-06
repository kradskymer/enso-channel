<<<<<<< HEAD
use enso_channel::{fanout, ChanWritePermit, ChanWritePermits, ChannelSender};
=======
use enso_channel::{fanout, ChannelSender, WritePermit, WritePermits};
>>>>>>> dd4e9c5 (refine examples and readme for the API changes)

fn main() {
    let (mut tx, [mut rx0, mut rx1]) = fanout::channel(16);
    // send a single value
    tx.try_send(42).unwrap();

    // reserve and send
    let mut permits = tx.try_send_at_most(16).unwrap();
    let mut i = 100;
    while let Some(permit) = permits.next() {
        permit.write(i);
        i += 1;
    }
    permits.commit();

    // receive
    assert_eq!(*rx0.try_recv().unwrap(), 42);
    assert_eq!(*rx1.try_recv().unwrap(), 42);

    // receive the rest
    for i in 100..100 + 15 {
        assert_eq!(*rx0.try_recv().unwrap(), i);
        assert_eq!(*rx1.try_recv().unwrap(), i);
    }
}
