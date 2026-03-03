#[macro_use]
mod harness;

use harness::contracts::InduceUncommittedSend;
use harness::mpmc as hm;
use harness::shared::{Channel, RecvBatchU32};
use std::panic::{catch_unwind, AssertUnwindSafe};

struct MpmcChan;

impl Channel for MpmcChan {
    type Sender = enso_channel::mpmc::Sender<u32>;
    type Receiver = enso_channel::mpmc::Receiver<u32>;

    type RecvBatch<'a>
        = enso_channel::mpmc::RecvIter<'a, u32>
    where
        Self::Receiver: 'a;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        enso_channel::mpmc::channel::<u32>(capacity)
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

    fn try_recv_many_batch<'a>(
        receiver: &'a mut Self::Receiver,
        n: usize,
    ) -> Result<Self::RecvBatch<'a>, enso_channel::errors::TryRecvError> {
        receiver.try_recv_many(n)
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

    fn try_recv_at_most_batch<'a>(
        receiver: &'a mut Self::Receiver,
        limit: usize,
    ) -> Result<Self::RecvBatch<'a>, enso_channel::errors::TryRecvAtMostError> {
        receiver.try_recv_at_most(limit)
    }
}

impl hm::MpmcChannel for MpmcChan {}

impl hm::CloneSender for MpmcChan {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

impl hm::CloneReceiver for MpmcChan {
    fn clone_receiver(receiver: &Self::Receiver) -> Self::Receiver {
        receiver.clone()
    }
}

impl InduceUncommittedSend for MpmcChan {
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

impl<'a> RecvBatchU32 for enso_channel::mpmc::RecvIter<'a, u32> {
    fn to_vec(&self) -> Vec<u32> {
        self.iter().copied().collect()
    }

    fn finish(self) {
        enso_channel::mpmc::RecvIter::finish(self)
    }
}

// ============================================================================
// Contract tests via macro
// ============================================================================

// Note: MPMC channels have different backpressure semantics than mpsc/broadcast.
// Items are freed once *any* consumer commits them, and the current implementation
// may allow claiming ahead. Therefore, we don't test contract_capacity_backpressure
// for these MPMC tests.

generate_contract_tests_prefixed!(
    mpmc,
    MpmcChan,
    [
        contract_fifo_order,
        contract_recv_empty,
        contract_recv_many_batch_iter_yields_fifo,
        contract_recv_batch_holds_capacity_until_commit,
        contract_recv_batch_finish_commits_immediately,
        contract_recv_at_most_batch_is_bounded,
        contract_recv_batch_drop_commits_without_iteration,
        contract_publisher_drop_disconnects_consumers,
        contract_publisher_drop_disconnects_consumers_with_uncommitted_claims,
        contract_consumer_drop_disconnects_publishers,
    ]
);

// ============================================================================
// MPMC-specific tests
// ============================================================================

#[test]
fn mpmc_basic_send_recv() {
    hm::basic_send_recv::<MpmcChan>();
}

#[test]
fn mpmc_sender_and_receiver_are_cloneable() {
    let (tx, rx) = <MpmcChan as Channel>::channel(8);
    let _tx2 = <MpmcChan as hm::CloneSender>::clone_sender(&tx);
    let _rx2 = <MpmcChan as hm::CloneReceiver>::clone_receiver(&rx);
}

#[test]
fn mpmc_item_delivered_to_exactly_one_consumer() {
    hm::item_delivered_to_exactly_one_of_two_consumers::<MpmcChan>();
}

#[test]
fn mpmc_consumers_split_work() {
    hm::consumers_split_work::<MpmcChan>();
}

// ============================================================================
// Concurrency tests
// ============================================================================

impl harness::concurrency::CloneSender for MpmcChan {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

impl harness::concurrency::CloneReceiver for MpmcChan {
    fn clone_receiver(receiver: &Self::Receiver) -> Self::Receiver {
        receiver.clone()
    }
}

#[test]
fn mpmc_concurrent_publisher_consumer() {
    harness::concurrency::contract_concurrent_single_producer::<MpmcChan>();
}

#[test]
fn mpmc_concurrent_multi_publisher() {
    harness::concurrency::contract_concurrent_mpsc::<MpmcChan>(4);
}

#[test]
fn mpmc_concurrent_work_stealing() {
    harness::concurrency::contract_concurrent_work_stealing::<MpmcChan>(4);
}

#[test]
fn mpmc_concurrent_full() {
    harness::concurrency::contract_concurrent_mpmc_work_stealing::<MpmcChan>(3, 3);
}
