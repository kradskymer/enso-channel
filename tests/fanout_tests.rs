use enso_channel::{
    errors::TrySendError, fanout, slot_recycler::ResetWithDefault, ChanReadRefs, ChanReceiver,
    ChanSender, ChanWritePermit, ChanWritePermits,
};
use rstest::{fixture, rstest};

const DEFAULT_CHANNEL_SIZE: usize = 16;
const RX_NUM: usize = 3;

#[fixture]
fn channel(
    #[default(DEFAULT_CHANNEL_SIZE)] channel_size: usize,
) -> (fanout::Sender<RX_NUM, usize>, Vec<fanout::Receiver<usize>>) {
    let (tx, [rx1, rx2, rx3]) = fanout::channel_with(channel_size, || 0).unwrap();
    (tx, vec![rx1, rx2, rx3])
}

#[rstest]
fn test_producer_block_by_slowest_receiver(
    #[from(channel)] (mut tx, mut rxs): (fanout::Sender<3, usize>, Vec<fanout::Receiver<usize>>),
) {
    let mut permits = tx
        .try_send_at_most(DEFAULT_CHANNEL_SIZE, ResetWithDefault)
        .unwrap();

    for i in 0..DEFAULT_CHANNEL_SIZE {
        if let Some(permit) = permits.next() {
            permit.write(i);
        }
    }
    permits.commit();

    assert!(matches!(
        tx.try_send(DEFAULT_CHANNEL_SIZE),
        Err(TrySendError::Full(DEFAULT_CHANNEL_SIZE))
    ));

    for rx in rxs.iter_mut().take(RX_NUM - 1) {
        let batch = rx.try_recv_at_most(DEFAULT_CHANNEL_SIZE).unwrap();
        batch
            .iter()
            .zip(0..DEFAULT_CHANNEL_SIZE)
            .for_each(|(item, expected)| assert_eq!(*item, expected));
    }

    assert!(matches!(
        tx.try_send(DEFAULT_CHANNEL_SIZE),
        Err(TrySendError::Full(DEFAULT_CHANNEL_SIZE))
    ));

    // auto commit on drop
    let _ = rxs[RX_NUM - 1].try_recv_at_most(DEFAULT_CHANNEL_SIZE);

    tx.try_send(DEFAULT_CHANNEL_SIZE)
        .expect("should send success");
}
