#[macro_use]
mod harness;

use harness::contracts::InduceUncommittedClaim;
use harness::queue as hq;
use harness::shared::Channel;
use std::panic::{AssertUnwindSafe, catch_unwind};

// ============================================================================
// Queue SPMC (Single Publisher, Multiple Consumer Work-Stealing)
// ============================================================================

struct QueueSpmc;

impl Channel for QueueSpmc {
    type Sender = enso_channel::queue::spmc::Sender<u32>;
    type Receiver = enso_channel::queue::spmc::Receiver<u32>;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        enso_channel::queue::spmc::channel::<u32>(capacity)
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

    fn try_recv_many(
        receiver: &mut Self::Receiver,
        max_items: usize,
    ) -> Result<Vec<u32>, enso_channel::errors::TryRecvError> {
        let guards = receiver.try_recv_many(max_items)?;
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
        let guards = receiver.try_recv_at_most(max_items)?;
        Ok(guards.copied().collect())
    }
}

impl hq::QueueChannel for QueueSpmc {}

impl hq::CloneReceiver for QueueSpmc {
    fn clone_receiver(receiver: &Self::Receiver) -> Self::Receiver {
        receiver.clone()
    }
}

impl InduceUncommittedClaim for QueueSpmc {
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

// ============================================================================
// Queue MPMC (Multiple Publisher, Multiple Consumer Work-Stealing)
// ============================================================================

struct QueueMpmc;

impl Channel for QueueMpmc {
    type Sender = enso_channel::queue::mpmc::Sender<u32>;
    type Receiver = enso_channel::queue::mpmc::Receiver<u32>;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        enso_channel::queue::mpmc::channel::<u32>(capacity)
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

    fn try_recv_many(
        receiver: &mut Self::Receiver,
        max_items: usize,
    ) -> Result<Vec<u32>, enso_channel::errors::TryRecvError> {
        let guards = receiver.try_recv_many(max_items)?;
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
        let guards = receiver.try_recv_at_most(max_items)?;
        Ok(guards.copied().collect())
    }
}

impl hq::QueueChannel for QueueMpmc {}

impl hq::CloneSender for QueueMpmc {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

impl hq::CloneReceiver for QueueMpmc {
    fn clone_receiver(receiver: &Self::Receiver) -> Self::Receiver {
        receiver.clone()
    }
}

impl InduceUncommittedClaim for QueueMpmc {
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

// ============================================================================
// Contract tests via macro
// ============================================================================

// Note: Queue channels have different backpressure semantics than exclusive/fanout.
// Items are freed once *any* consumer commits them, and the current implementation
// may allow claiming ahead. Therefore, we don't test contract_capacity_backpressure
// for queue channels.

generate_contract_tests_prefixed!(
    queue_spmc,
    QueueSpmc,
    [
        contract_fifo_order,
        contract_recv_empty,
        contract_publisher_drop_disconnects_consumers,
        contract_publisher_drop_disconnects_consumers_with_uncommitted_claims,
        contract_consumer_drop_disconnects_publishers,
    ]
);

generate_contract_tests_prefixed!(
    queue_mpmc,
    QueueMpmc,
    [
        contract_fifo_order,
        contract_recv_empty,
        contract_publisher_drop_disconnects_consumers,
        contract_publisher_drop_disconnects_consumers_with_uncommitted_claims,
        contract_consumer_drop_disconnects_publishers,
    ]
);

// ============================================================================
// Queue-specific tests
// ============================================================================

#[test]
fn queue_spmc_basic_send_recv() {
    hq::basic_send_recv::<QueueSpmc>();
}

#[test]
fn queue_mpmc_basic_send_recv() {
    hq::basic_send_recv::<QueueMpmc>();
}

#[test]
fn queue_mpmc_sender_and_receiver_are_cloneable() {
    let (tx, rx) = <QueueMpmc as Channel>::channel(8);
    let _tx2 = <QueueMpmc as hq::CloneSender>::clone_sender(&tx);
    let _rx2 = <QueueMpmc as hq::CloneReceiver>::clone_receiver(&rx);
}

#[test]
fn queue_spmc_item_delivered_to_exactly_one_consumer() {
    hq::item_delivered_to_exactly_one_of_two_consumers::<QueueSpmc>();
}

#[test]
fn queue_mpmc_item_delivered_to_exactly_one_consumer() {
    hq::item_delivered_to_exactly_one_of_two_consumers::<QueueMpmc>();
}

#[test]
fn queue_spmc_consumers_split_work() {
    hq::consumers_split_work::<QueueSpmc>();
}

#[test]
fn queue_mpmc_consumers_split_work() {
    hq::consumers_split_work::<QueueMpmc>();
}

// ============================================================================
// Concurrency tests
// ============================================================================

impl harness::concurrency::CloneReceiver for QueueSpmc {
    fn clone_receiver(receiver: &Self::Receiver) -> Self::Receiver {
        receiver.clone()
    }
}

impl harness::concurrency::CloneSender for QueueMpmc {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

impl harness::concurrency::CloneReceiver for QueueMpmc {
    fn clone_receiver(receiver: &Self::Receiver) -> Self::Receiver {
        receiver.clone()
    }
}

#[test]
fn queue_spmc_concurrent_publisher_consumer() {
    harness::concurrency::contract_concurrent_spsc::<QueueSpmc>();
}

#[test]
fn queue_spmc_concurrent_work_stealing() {
    harness::concurrency::contract_concurrent_work_stealing::<QueueSpmc>(4);
}

#[test]
fn queue_mpmc_concurrent_publisher_consumer() {
    harness::concurrency::contract_concurrent_spsc::<QueueMpmc>();
}

#[test]
fn queue_mpmc_concurrent_multi_publisher() {
    harness::concurrency::contract_concurrent_mpsc::<QueueMpmc>(4);
}

#[test]
fn queue_mpmc_concurrent_work_stealing() {
    harness::concurrency::contract_concurrent_work_stealing::<QueueMpmc>(4);
}

#[test]
fn queue_mpmc_concurrent_full() {
    harness::concurrency::contract_concurrent_mpmc_work_stealing::<QueueMpmc>(3, 3);
}
