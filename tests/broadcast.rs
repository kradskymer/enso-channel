#[macro_use]
mod harness;

use harness::broadcast as h;
use harness::contracts::InduceUncommittedClaim;
use harness::shared::Channel;
use std::panic::{AssertUnwindSafe, catch_unwind};

struct BroadcastFanout2;

impl h::BroadcastAdapter<2> for BroadcastFanout2 {
    type Sender = enso_channel::broadcast::Sender<u32, 2>;
    type Receiver = enso_channel::broadcast::Receiver<u32>;

    fn channel_broadcast(capacity: usize) -> (Self::Sender, [Self::Receiver; 2]) {
        enso_channel::broadcast::channel::<u32, 2>(capacity)
    }

    fn try_send(
        sender: &mut Self::Sender,
        item: u32,
    ) -> Result<(), enso_channel::errors::TrySendError> {
        sender.try_send(item)
    }

    fn try_recv(receiver: &mut Self::Receiver) -> Result<u32, enso_channel::errors::TryRecvError> {
        let guard = receiver.try_recv()?;
        Ok(*guard)
    }
}

struct BroadcastContractReceiver(enso_channel::broadcast::Receiver<u32>);

/// Adapter for contract tests: uses only the first receiver.
struct BroadcastContractChan;

impl Channel for BroadcastContractChan {
    type Sender = enso_channel::broadcast::Sender<u32, 2>;
    type Receiver = BroadcastContractReceiver;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        let (tx, rxs) = enso_channel::broadcast::channel::<u32, 2>(capacity);
        let [rx0, rx1] = rxs;
        drop(rx1);
        (tx, BroadcastContractReceiver(rx0))
    }

    fn try_send(
        sender: &mut Self::Sender,
        item: u32,
    ) -> Result<(), enso_channel::errors::TrySendError> {
        sender.try_send(item)
    }

    fn try_recv(receiver: &mut Self::Receiver) -> Result<u32, enso_channel::errors::TryRecvError> {
        let guard = receiver.0.try_recv()?;
        Ok(*guard)
    }

    fn try_recv_many(
        receiver: &mut Self::Receiver,
        max_items: usize,
    ) -> Result<Vec<u32>, enso_channel::errors::TryRecvError> {
        let guards = receiver.0.try_recv_many(max_items)?;
        Ok(guards.copied().collect())
    }

    fn try_send_many(
        sender: &mut Self::Sender,
        items: &[u32],
    ) -> Result<(), enso_channel::errors::TrySendError> {
        let mut batch = sender.try_send_many(items.len(), || 0)?;
        batch.write_exact(items.iter().copied());
        Ok(())
    }

    fn try_send_at_most(
        sender: &mut Self::Sender,
        items: &[u32],
    ) -> Result<usize, enso_channel::errors::TrySendAtMostError> {
        let mut batch = sender.try_send_at_most(items.len(), || 0)?;
        let n = batch.capacity();
        batch.write_exact(items.iter().take(n).copied());
        Ok(n)
    }

    fn try_recv_at_most(
        receiver: &mut Self::Receiver,
        max_items: usize,
    ) -> Result<Vec<u32>, enso_channel::errors::TryRecvAtMostError> {
        let guards = receiver.0.try_recv_at_most(max_items)?;
        Ok(guards.copied().collect())
    }
}

impl InduceUncommittedClaim for BroadcastContractChan {
    fn induce_uncommitted_claim(sender: &mut Self::Sender, n: usize) {
        fn panic_factory() -> u32 {
            panic!("induced panic to create uncommitted claim")
        }

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = sender.try_send_many(n, panic_factory);
        }));

        assert!(
            result.is_err(),
            "expected panic while inducing uncommitted claim"
        );
    }
}

generate_contract_tests_prefixed!(
    broadcast,
    BroadcastContractChan,
    [
        contract_fifo_order,
        contract_capacity_backpressure,
        contract_recv_empty,
        contract_publisher_drop_disconnects_consumers,
        contract_publisher_drop_disconnects_consumers_with_uncommitted_claims,
        contract_consumer_drop_disconnects_publishers,
    ]
);

#[test]
fn broadcast_each_receiver_sees_all_items() {
    h::each_receiver_sees_all_items::<2, BroadcastFanout2>();
}

#[test]
fn broadcast_backpressured_by_slowest() {
    h::publisher_is_backpressured_by_slowest::<BroadcastFanout2>();
}

#[test]
fn broadcast_publisher_drop_disconnects_consumers() {
    h::publisher_drop_disconnects_consumers::<BroadcastFanout2, 2>();
}

impl harness::concurrency::CloneSender for BroadcastContractChan {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

#[test]
fn broadcast_concurrent_publisher_consumer() {
    harness::concurrency::contract_concurrent_single_producer::<BroadcastContractChan>();
}

#[test]
fn broadcast_concurrent_multi_publisher() {
    harness::concurrency::contract_concurrent_mpsc::<BroadcastContractChan>(4);
}
