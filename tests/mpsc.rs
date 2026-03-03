#[macro_use]
mod harness;

use harness::mpsc as h;
use harness::shared::Channel;
use std::panic::{catch_unwind, AssertUnwindSafe};

struct MpscChan;

impl Channel for MpscChan {
    type Sender = enso_channel::mpsc::Sender<u32>;
    type Receiver = enso_channel::mpsc::Receiver<u32>;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        enso_channel::mpsc::channel::<u32>(capacity)
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

impl h::MpscChannel for MpscChan {
    fn try_send_batch_write_exact(
        sender: &mut Self::Sender,
        items: &[u32],
    ) -> Result<(), enso_channel::errors::TrySendError> {
        let mut batch = sender.try_send_many(items.len(), || 0)?;
        batch.write_exact(items.iter().copied());
        batch.finish();
        Ok(())
    }
}

impl h::CloneSender for MpscChan {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

impl harness::contracts::InduceUncommittedClaim for MpscChan {
    fn induce_uncommitted_claim(sender: &mut Self::Sender, n: usize) {
        fn panic_factory() -> u32 {
            panic!("intentional panic to skip publish")
        }

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _batch = sender
                .try_send_many(n, panic_factory)
                .expect("claim should succeed");
            // Drop triggers filling via factory, which panics before commit.
        }));

        assert!(result.is_err(), "expected panic from SendBatch drop");
    }
}

// ============================================================================
// Contract tests via macro
// ============================================================================

generate_contract_tests_prefixed!(
    mpsc,
    MpscChan,
    [
        contract_fifo_order,
        contract_capacity_backpressure,
        contract_recv_empty,
        contract_publisher_drop_disconnects_consumers,
        contract_publisher_drop_disconnects_consumers_with_uncommitted_claims,
        contract_consumer_drop_disconnects_publishers,
    ]
);

// ============================================================================
// MPSC-specific tests
// ============================================================================

#[test]
fn mpsc_send_batch_write_exact() {
    h::send_batch_write_exact::<MpscChan>();
}

#[test]
fn mpsc_sender_is_cloneable() {
    h::sender_is_cloneable::<MpscChan>();
}

// ============================================================================
// Concurrency tests
// ============================================================================

impl harness::concurrency::CloneSender for MpscChan {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

#[test]
fn mpsc_concurrent_producer_consumer() {
    harness::concurrency::contract_concurrent_single_producer::<MpscChan>();
}

#[test]
fn mpsc_concurrent_multi_producer() {
    harness::concurrency::contract_concurrent_mpsc::<MpscChan>(4);
}
