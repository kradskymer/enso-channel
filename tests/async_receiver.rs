use std::{thread::JoinHandle, time::Duration};

use enso_channel::{fanout, mpsc, slot_recycler::ResetWithDefault, ChanReadRefs, ChanWritePermits};
#[cfg(test)]
use enso_channel::{ChanReceiver, ChanSender};
use rstest::{fixture, rstest};

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

#[allow(clippy::enum_variant_names)]
enum SendMethod {
    TrySend,
    TryReserve,
    TrySendAtMost,
}

fn spawn_sender<S: ChanSender<usize> + 'static + Send>(
    mut tx: S,
    method: SendMethod,
) -> JoinHandle<()> {
    std::thread::spawn(move || match method {
        SendMethod::TrySend => {
            for i in 0..DEFAULT_CHANNEL_SIZE {
                tx.try_send(i).unwrap();
            }
        }
        SendMethod::TryReserve => {
            for i in 0..DEFAULT_CHANNEL_SIZE {
                use enso_channel::ChanWritePermit;

                let permit = tx.try_reserve(ResetWithDefault).unwrap();
                permit.write(i);
            }
        }
        SendMethod::TrySendAtMost => {
            let mut permits = tx
                .try_send_at_most(DEFAULT_CHANNEL_SIZE, ResetWithDefault)
                .unwrap();
            for i in 0..DEFAULT_CHANNEL_SIZE {
                use enso_channel::ChanWritePermit;

                let permit = permits.next().unwrap();
                permit.write(i);
            }
            permits.commit();
        }
    })
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_receiver_recv_at_most_async_with_zero_limit_return_immediately<
    S: ChanSender<usize> + 'static + Send,
    R: ChanReceiver<usize>,
>(
    #[case] (_tx, mut rxs): (S, Vec<R>),
) {
    let mut rx = rxs.pop().unwrap();
    let poll = async {
        let result = rx.recv_at_most_async(0).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().iter().count(), 0);
    };
    pollster::block_on(poll);
}

#[rstest]
#[case::mpsc_try_send(mpsc_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TrySend)]
#[case::mpsc_try_reserve(mpsc_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TryReserve)]
#[case::mpsc_try_send_at_most(mpsc_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TrySendAtMost)]
#[case::fanout_try_send(fanout_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TrySend)]
#[case::fanout_try_reserve(fanout_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TryReserve)]
#[case::fanout_try_send_at_most(fanout_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TrySendAtMost)]
fn test_receiver_recv_async<S: ChanSender<usize> + 'static + Send, R: ChanReceiver<usize>>(
    #[case] (tx, mut rxs): (S, Vec<R>),
    #[case] method: SendMethod,
) {
    let mut rx = rxs.pop().unwrap();
    let poll = async {
        let mut i = 0;
        loop {
            if let Some(r) = rx.recv_async().await {
                assert_eq!(*r, i);
                i += 1;
            } else {
                break;
            }
        }
    };
    let handle = spawn_sender(tx, method);
    pollster::block_on(poll);
    handle.join().unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[case::mpsc_try_send(mpsc_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TrySend)]
#[case::mpsc_try_reserve(mpsc_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TryReserve)]
#[case::mpsc_try_send_at_most(mpsc_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TrySendAtMost)]
#[case::fanout_try_send(fanout_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TrySend)]
#[case::fanout_try_reserve(fanout_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TryReserve)]
#[case::fanout_try_send_at_most(fanout_channel(DEFAULT_CHANNEL_SIZE), SendMethod::TrySendAtMost)]
fn test_receiver_recv_at_most_async<
    S: ChanSender<usize> + 'static + Send,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
    #[case] method: SendMethod,
) {
    let mut rx = rxs.pop().unwrap();
    let poll = async {
        let mut received = Vec::new();
        loop {
            match rx.recv_at_most_async(DEFAULT_CHANNEL_SIZE).await {
                Some(refs) => {
                    // recv_at_most_async may return a partial batch when the
                    // sender is still publishing concurrently.
                    let batch: Vec<_> = refs.iter().copied().collect();
                    assert!(
                        !batch.is_empty(),
                        "recv_at_most_async should not return empty batches"
                    );
                    received.extend(batch);
                }
                None => break,
            }
        }
        received
    };

    let handle = spawn_sender(tx, method);

    let received = pollster::block_on(poll);
    let expected: Vec<_> = (0..DEFAULT_CHANNEL_SIZE).collect();
    assert_eq!(received, expected, "should receive all sent items in order");
    handle.join().unwrap();
}

#[rstest]
#[timeout(Duration::from_secs(5))]
#[case::mpsc(mpsc_channel(DEFAULT_CHANNEL_SIZE))]
#[case::fanout(fanout_channel(DEFAULT_CHANNEL_SIZE))]
fn test_async_receiver_notified_on_sender_shutdown<
    S: ChanSender<usize> + 'static + Send,
    R: ChanReceiver<usize>,
>(
    #[case] (tx, mut rxs): (S, Vec<R>),
) {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let barrier_clone = barrier.clone();
    let handle = std::thread::spawn(move || {
        barrier_clone.wait();
        drop(tx);
    });
    let mut rx = rxs.pop().unwrap();
    let poll = async {
        barrier.wait();
        while let Some(refs) = rx.recv_at_most_async(DEFAULT_CHANNEL_SIZE).await {
            refs.finish();
        }
    };
    pollster::block_on(poll);
    handle.join().unwrap();
}
