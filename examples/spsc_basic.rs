use enso_channel::{ChanReadRefs, ChanReceiver, ChanWritePermit, ChanWritePermits, ChannelSender};

fn main() {
    use enso_channel::mpsc;

    let (mut tx, mut rx) = mpsc::channel::<u64>(64).unwrap();

    // Single send/recv
    tx.try_send(42).unwrap();
    {
        let guard = rx.try_recv().unwrap();
        assert_eq!(*guard, 42);
        // Guard should release on drop, allowing the next item to be observed.
    }

    // Batch send
    let mut batch = tx.try_send_at_most(8).unwrap();
    let mut i = 0;
    while let Some(batch) = batch.next() {
        i += 1;
        batch.write(i);
    }
    batch.commit();

    // Batch send with at most semantics.
    let mut batch = tx.try_send_at_most(8).unwrap();
    let mut i = 0;
    while let Some(batch) = batch.next() {
        i += 1;
        batch.write(i);
    }
    batch.commit();

    // Batch recv
    {
        let batch = rx.try_recv_at_most(8).unwrap();
        let mut count = 0;
        for val in batch.iter() {
            count += 1;
            println!("{val}");
        }
        assert_eq!(count, 8);
    }

    // Batch recv with at most semantics.
    {
        let batch = rx.try_recv_at_most(20).unwrap();
        let mut count = 0;
        for val in batch.iter() {
            count += 1;
            println!("{val}");
        }
        assert_eq!(count, 8);
    }
}
