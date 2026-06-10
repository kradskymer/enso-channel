use enso_channel::{
    errors::{InvalidChannelSize, TryRecvError, TryReserveError, TrySendError},
    fanout, mpsc,
    slot_recycler::{ResetWith, ResetWithDefault},
    ChanReadRefs, ChanWritePermit, ChanWritePermits,
};
use enso_channel::{ChanReceiver, ChanSender};
use rstest::{fixture, rstest};

#[rstest]
#[case(0, InvalidChannelSize::NotAPowerOfTwo)]
#[case(3, InvalidChannelSize::NotAPowerOfTwo)]
#[case(usize::MAX, InvalidChannelSize::TooLarge)]
fn test_invalid_channel_size_should_fail(
    #[case] channel_size: usize,
    #[case] err: InvalidChannelSize,
) {
    assert!(matches!(mpsc::channel::<usize>(channel_size), Err(e) if e == err));
    assert!(matches!(mpsc::channel_with::<usize, _>(channel_size, || 0), Err(e) if e == err));

    assert!(matches!(fanout::channel::<2, usize>(channel_size), Err(e) if e == err));
    assert!(matches!(fanout::channel_with::<2, usize, _>(channel_size, || 0), Err(e) if e == err));
}

const DEFAULT_CHANNEL_SIZE: usize = 16;

#[fixture]
fn mpsc_channel(
    #[default(DEFAULT_CHANNEL_SIZE)] channel_size: usize,
) -> (mpsc::Sender<usize>, Vec<mpsc::Receiver<usize>>) {
    let (tx, rx) = mpsc::channel_with(channel_size, || 0).unwrap();
    (tx, vec![rx])
}

#[fixture]
fn fanout_channel(
    #[default(DEFAULT_CHANNEL_SIZE)] channel_size: usize,
) -> (fanout::Sender<3, usize>, Vec<fanout::Receiver<usize>>) {
    let (tx, [rx1, rx2, rx3]) = fanout::channel_with(channel_size, || 0).unwrap();
    (tx, vec![rx1, rx2, rx3])
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_back_pressure_applied<S: ChanSender<usize> + Clone, R: ChanReceiver<usize>>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    use enso_channel::{
        errors::{TryReserveError, TrySendAtMostError, TrySendError},
        slot_recycler::ResetWithDefault,
    };

    (0..DEFAULT_CHANNEL_SIZE)
        .zip(std::iter::repeat(tx.clone()))
        .for_each(|(i, tx)| {
            tx.try_send(i).unwrap();
        });
    assert_eq!(
        tx.try_send(DEFAULT_CHANNEL_SIZE),
        Err(TrySendError::Full(DEFAULT_CHANNEL_SIZE))
    );
    assert!(matches!(
        tx.try_reserve(ResetWithDefault),
        Err(TryReserveError::Full)
    ));
    assert!(matches!(
        tx.try_send_at_most(5, ResetWithDefault),
        Err(TrySendAtMostError::Full)
    ));

    for rx in rxs.iter_mut() {
        assert!(rx.try_recv().is_ok());
    }

    assert!(tx.try_send(DEFAULT_CHANNEL_SIZE).is_ok());
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_empty_channel_recv<S: ChanSender<usize>, R: ChanReceiver<usize>>(
    #[case] (_tx, mut rxs): (S, Vec<R>),
) {
    for rx in rxs.iter_mut() {
        use enso_channel::errors::TryRecvError;

        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));

        assert!(matches!(rx.try_recv_at_most(10), Err(TryRecvError::Empty)));
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_ordering_should_be_fifo<S: ChanSender<usize>, R: ChanReceiver<usize>>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    for i in 0..DEFAULT_CHANNEL_SIZE {
        tx.try_send(i).unwrap();
    }
    for rx in rxs.iter_mut() {
        for i in 0..DEFAULT_CHANNEL_SIZE {
            assert_eq!(*rx.try_recv().unwrap(), i);
        }
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_slot_recycler_applied_on_write_permits_drop<
    S: ChanSender<usize>,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    const MAGIC_NUMBER: usize = 933536831074;
    let permit = tx.try_reserve(|i: &mut _| *i = MAGIC_NUMBER).unwrap();
    // drop without write
    drop(permit);

    // read should return the magic number
    for rx in rxs.iter_mut() {
        assert_eq!(*rx.try_recv().unwrap(), MAGIC_NUMBER);
    }

    const ANOTHER_MAGIC_NUMBER: usize = 265884630365;
    let permits = tx
        .try_send_at_most(DEFAULT_CHANNEL_SIZE, |i: &mut _| *i = ANOTHER_MAGIC_NUMBER)
        .unwrap();
    permits.commit();

    for rx in rxs.iter_mut() {
        let batch = rx.try_recv_at_most(DEFAULT_CHANNEL_SIZE).unwrap();
        for value in batch.iter() {
            assert_eq!(*value, ANOTHER_MAGIC_NUMBER);
        }
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_read_refs_block_sender_until_commit<S: ChanSender<usize>, R: ChanReceiver<usize>>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    const MAGIC_NUMBER: usize = 525770450318;

    let mut permits = tx
        .try_send_at_most(DEFAULT_CHANNEL_SIZE, |i: &mut _| *i = MAGIC_NUMBER)
        .unwrap();
    for i in 0..DEFAULT_CHANNEL_SIZE {
        if let Some(permit) = permits.next() {
            use enso_channel::ChanWritePermit;
            permit.update_in_place(|v| *v = i);
        }
    }
    permits.commit();

    // single ref
    let mut refs = vec![];
    for rx in rxs.iter_mut() {
        refs.push(rx.try_recv().unwrap());
    }
    assert!(tx.try_send(DEFAULT_CHANNEL_SIZE).is_err());
    drop(refs);
    assert!(tx.try_send(DEFAULT_CHANNEL_SIZE).is_ok());

    // batch ref
    let mut refs = vec![];
    for rx in rxs.iter_mut() {
        refs.push(rx.try_recv_at_most(DEFAULT_CHANNEL_SIZE).unwrap());
    }

    assert!(tx.try_send(DEFAULT_CHANNEL_SIZE + 1).is_err());
    drop(refs);
    assert!(tx.try_send(DEFAULT_CHANNEL_SIZE + 1).is_ok());
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_uncommitted_permits_will_not_be_recv<S: ChanSender<usize>, R: ChanReceiver<usize>>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    let mut permits = tx
        .try_send_at_most(100, |i: &mut _| *i = usize::MAX)
        .unwrap();
    for i in 0..permits.total_reserved() {
        let permit = permits.next().unwrap();
        permit.write(i);
    }

    for rx in rxs.iter_mut() {
        assert!(matches!(rx.try_recv_at_most(100), Err(TryRecvError::Empty)));
    }

    permits.commit();
    for rx in rxs.iter_mut() {
        let refs = rx.try_recv_at_most(100).unwrap();
        refs.iter().enumerate().for_each(|(i, v)| assert_eq!(*v, i));
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_drop_all_sender_disconnect_receivers<
    S: ChanSender<usize> + Clone,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    let tx_clone = tx.clone();
    tx.try_send(1).unwrap();
    drop(tx);
    for rx in rxs.iter_mut() {
        assert_eq!(*rx.try_recv().unwrap(), 1);
    }
    for rx in rxs.iter_mut() {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    drop(tx_clone);

    for rx in rxs.iter_mut() {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
    }

    for rx in rxs.iter_mut() {
        assert!(matches!(
            rx.try_recv_at_most(DEFAULT_CHANNEL_SIZE),
            Err(TryRecvError::Disconnected)
        ));
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_drop_all_receiver_disconnect_sender<S: ChanSender<usize>, R: ChanReceiver<usize>>(
    #[case] (tx, rxs): (S, Vec<R>),
) {
    use enso_channel::errors::{TryReserveError, TrySendAtMostError};

    tx.try_send(1).unwrap();
    drop(rxs);

    assert!(matches!(tx.try_send(2), Err(TrySendError::Disconnected(2))));
    assert!(matches!(
        tx.try_reserve(ResetWithDefault),
        Err(TryReserveError::Disconnected)
    ));
    assert!(matches!(
        tx.try_send_at_most(DEFAULT_CHANNEL_SIZE, ResetWithDefault),
        Err(TrySendAtMostError::Disconnected)
    ));
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_try_send_at_most_is_bounded<S: ChanSender<usize>, R: ChanReceiver<usize>>(
    #[case] (tx, _rxs): (S, Vec<R>),
) {
    let mut permits = tx.try_send_at_most(5, ResetWithDefault).unwrap();
    assert_eq!(permits.total_reserved(), 5);
    for i in 10..10 + 5 {
        let permit = permits.next().unwrap();
        permit.write(i);
    }
    assert!(permits.next().is_none());
    permits.commit();

    let mut permits = tx.try_send_at_most(100, ResetWithDefault).unwrap();
    assert_eq!(permits.total_reserved(), DEFAULT_CHANNEL_SIZE - 5);
    for _ in 0..permits.total_reserved() {
        assert!(permits.next().is_some());
    }
    assert!(permits.next().is_none());
    permits.commit();
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_try_recv_at_most_is_bounded<S: ChanSender<usize>, R: ChanReceiver<usize>>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    let mut permits = tx.try_send_at_most(100, ResetWithDefault).unwrap();
    for i in 0..permits.total_reserved() {
        let permit = permits.next().unwrap();
        permit.write(i);
    }
    permits.commit();

    for rx in rxs.iter_mut() {
        let refs = rx.try_recv_at_most(10).unwrap();
        assert_eq!(refs.iter().count(), 10);
    }

    for rx in rxs.iter_mut() {
        let refs = rx.try_recv_at_most(100).unwrap();
        assert_eq!(refs.iter().count(), DEFAULT_CHANNEL_SIZE - 10);
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_try_send_at_most_with_zero_limit_return_empty_permit<
    S: ChanSender<usize>,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    let mut permits = tx.try_send_at_most(0, ResetWithDefault).unwrap();
    assert_eq!(permits.total_reserved(), 0);
    assert!(permits.next().is_none());
    permits.commit();

    for rx in rxs.iter_mut() {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_try_recv_at_most_with_zero_limit_return_empty_permit<
    S: ChanSender<usize>,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    tx.try_send(1).unwrap();
    for rx in rxs.iter_mut() {
        let refs = rx.try_recv_at_most(0).unwrap();
        assert!(refs.iter().next().is_none());
    }
}

#[allow(clippy::enum_variant_names)]
enum PanicCause {
    WritePermit,
    WritePermitsSlot,
    WritePermitsCommit,
}

#[rstest]
#[case::mpsc_write_permit(mpsc_channel(DEFAULT_CHANNEL_SIZE), PanicCause::WritePermit)]
#[case::mpsc_write_permits_slot(mpsc_channel(DEFAULT_CHANNEL_SIZE), PanicCause::WritePermitsSlot)]
#[case::mpsc_write_permits_commit(
    mpsc_channel(DEFAULT_CHANNEL_SIZE),
    PanicCause::WritePermitsCommit
)]
#[case::fanout_write_permit(fanout_channel(DEFAULT_CHANNEL_SIZE), PanicCause::WritePermit)]
#[case::fanout_write_permits_slot(
    fanout_channel(DEFAULT_CHANNEL_SIZE),
    PanicCause::WritePermitsSlot
)]
#[case::fanout_write_permits_commit(
    fanout_channel(DEFAULT_CHANNEL_SIZE),
    PanicCause::WritePermitsCommit
)]
fn write_permit_panic_should_shutdown_channel<
    S: ChanSender<usize> + Clone + Send + 'static,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
    #[case] cause: PanicCause,
) {
    let panic_tx = tx.clone();
    let panic_recycler = ResetWith(|| panic!());
    let handle = std::thread::spawn(move || match cause {
        PanicCause::WritePermit => {
            let permit = panic_tx.try_reserve(panic_recycler).unwrap();
            drop(permit);
        }
        PanicCause::WritePermitsSlot => {
            let mut permits = panic_tx.try_send_at_most(10, panic_recycler).unwrap();
            let next = permits.next().unwrap();
            drop(next);
        }
        PanicCause::WritePermitsCommit => {
            let permits = panic_tx.try_send_at_most(10, panic_recycler).unwrap();
            drop(permits);
        }
    });
    let _ = handle.join();

    // assertion: the channel should be shutdown since it is poisoned
    assert!(matches!(
        tx.try_reserve(ResetWithDefault),
        Err(TryReserveError::Disconnected)
    ));

    // assertion: the channel should be shutdown since it is poisoned
    for rx in rxs.iter_mut() {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn write_permit_panic_allow_receiver_drain_already_published_before_shutdown<
    S: ChanSender<usize> + Clone + Send + 'static,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    for i in 1..5 {
        tx.try_send(i).unwrap();
    }
    let panic_tx = tx.clone();
    let panic_recycler = ResetWith(|| panic!());
    let handle = std::thread::spawn(move || {
        let mut permits = panic_tx
            .try_send_at_most(DEFAULT_CHANNEL_SIZE, panic_recycler)
            .unwrap();
        let next = permits.next().unwrap();
        drop(next);
    });
    let _ = handle.join();

    for rx in rxs.iter_mut() {
        for i in 1..3 {
            assert!(matches!(rx.try_recv(), Ok(v) if *v == i));
        }
        let refs = rx.try_recv_at_most(10).unwrap();
        assert_eq!(refs.iter().count(), 2);
        for (i, j) in refs.iter().zip(3..5) {
            assert_eq!(*i, j)
        }
        refs.finish();
        // assertion: the channel should be shutdown since it is poisoned
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
        assert!(matches!(
            rx.try_recv_at_most(10),
            Err(TryRecvError::Disconnected)
        ));
    }
}

#[rstest]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_write_permit_panic_will_prevent_subsequent_sent_data_receiving<
    S: ChanSender<usize> + Clone + Send + 'static,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    let tx_panic = tx.clone();
    let panic_recycler = ResetWith(|| panic!());
    let panic_tx_claimed = std::sync::Arc::new(std::sync::Barrier::new(2));
    let panic_tx_claimed_clone = panic_tx_claimed.clone();

    let safe_tx_claimed = std::sync::Arc::new(std::sync::Barrier::new(2));
    let safe_tx_claimed_clone = safe_tx_claimed.clone();
    let handle = std::thread::spawn(move || {
        let panic_permits = tx_panic.try_send_at_most(5, panic_recycler).unwrap();
        panic_tx_claimed_clone.wait();
        safe_tx_claimed_clone.wait();
        panic_permits.commit();
    });
    panic_tx_claimed.wait();
    let mut safe_permit = tx.try_send_at_most(5, ResetWithDefault).unwrap();
    safe_tx_claimed.wait();

    for i in 0..safe_permit.total_reserved() {
        let permit = safe_permit.next().unwrap();
        permit.write(i);
    }
    safe_permit.commit();

    // Wait for the panic thread to finish — the recycler panic is caught
    // internally and `terminate()` is called, so the thread exits normally.
    handle.join().unwrap();
    assert!(matches!(
        tx.try_send(10),
        Err(TrySendError::Disconnected(_))
    ));
    for rx in rxs.iter_mut() {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
    }
}
