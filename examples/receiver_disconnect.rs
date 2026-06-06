use enso_channel::{mpsc, ChanWritePermit, ChanWritePermits, ChannelSender};

fn main() {
    let (mut sender, receiver) = mpsc::channel::<u64>(16);
    let mut batch = sender.try_send_at_most(8).unwrap();
    // Disconnect receiver while batch guard is still alive.
    drop(receiver);
    // Batch guard should still be able to publish successfully, but items will never be observed.
    while let Some(batch) = batch.next() {
        batch.write(0);
    }
    batch.commit();
    // This time will return `Disconnected`
    let batch = sender.try_send_at_most(8);
    matches!(
        batch,
        Err(enso_channel::errors::TrySendAtMostError::Disconnected)
    );
}
