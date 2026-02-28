use enso_channel::exclusive::mpsc;

fn main() {
    let (mut sender, receiver) = mpsc::channel::<u64>(16);
    let mut batch = sender.try_send_many_default(8).unwrap();
    // Disconnect receiver while batch guard is still alive.
    drop(receiver);
    // Batch guard should still be able to publish successfully, but items will never be observed.
    batch.fill_with(|| 0);
    batch.finish();
    // This time will return `Disconnected`
    let batch = sender.try_send_many_default(8);
    matches!(batch, Err(enso_channel::errors::TrySendError::Disconnected));
}
