use enso_channel::{
    fanout, slot_recycler::ResetWithDefault, ChanReceiver, ChanSender, ChanWritePermit,
    ChanWritePermits,
};

fn main() {
    let (mut tx, [mut rx0, mut rx1]) = fanout::channel(16).unwrap();
    // send a single value
    tx.try_send(42).unwrap();

    // reserve and send
    let mut permits = tx.try_send_at_most(16, ResetWithDefault).unwrap();
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
