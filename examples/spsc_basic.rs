fn main() {
    use enso_channel::mpsc;

    let (mut tx, mut rx) = mpsc::channel::<u64>(64);

    // Single send/recv
    tx.try_send(42).unwrap();
    {
        let guard = rx.try_recv().unwrap();
        assert_eq!(*guard, 42);
        // Guard should release on drop, allowing the next item to be observed.
    }

    // Batch send
    let mut batch = tx.try_send_many_default(8).unwrap();
    for i in 1..=8 {
        batch.write_next(i);
    }
    batch.finish();

    // Batch send with at most semantics.
    let mut batch = tx.try_send_at_most(8, || 0).unwrap();
    batch.fill_with(|| 100);
    drop(batch);

    // Batch recv
    {
        let iter = rx.try_recv_many(8).unwrap();
        let mut count = 0;
        for val in iter.iter() {
            count += 1;
            println!("{val}");
        }
        assert_eq!(count, 8);
    }

    // Batch recv with at most semantics.
    {
        let iter = rx.try_recv_at_most(20).unwrap();
        let mut count = 0;
        for val in iter.iter() {
            count += 1;
            println!("{val}");
        }
        assert_eq!(count, 8);
    }
}
