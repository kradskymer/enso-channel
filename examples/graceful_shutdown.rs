use enso_channel::errors::TryRecvAtMostError;

fn main() {
    let (mut sender, mut receiver) = enso_channel::mpsc::channel::<u64>(16);
    sender.try_send(42).unwrap();
    // Drop sender to initiate shutdown.
    drop(sender);
    // Receiver can still drain already-committed items.
    assert_eq!(*receiver.try_recv().unwrap(), 42);
    // After draining, receiver observes `Disconnected`.
    loop {
        match receiver.try_recv_at_most(64) {
            Ok(iter) => {
                for v in iter.iter() {
                    let _ = *v;
                }
            }
            Err(TryRecvAtMostError::Empty) => {
                // retry/backoff/spin: caller-controlled
            }
            Err(TryRecvAtMostError::Disconnected) => break,
        }
    }
}
