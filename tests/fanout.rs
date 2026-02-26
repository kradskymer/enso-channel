#[macro_use]
mod harness;

use harness::contracts::InduceUncommittedClaim;
use harness::fanout as h;
use harness::shared::Channel;
use std::panic::{AssertUnwindSafe, catch_unwind};

// ============================================================================
// SPMC (Single Publisher, Multiple Consumer)
// ============================================================================

struct Spmc2;

impl h::FanoutAdapter<2> for Spmc2 {
    type Sender = enso_channel::fanout::spmc::Sender<u32, 2>;
    type Receiver = enso_channel::fanout::spmc::Receiver<u32>;

    fn channel_fanout(capacity: usize) -> (Self::Sender, [Self::Receiver; 2]) {
        enso_channel::fanout::spmc::channel::<u32, 2>(capacity)
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

/// Adapter for contract tests: uses only the first receiver.
struct Spmc2Single;

impl Channel for Spmc2Single {
    type Sender = enso_channel::fanout::spmc::Sender<u32, 2>;
    type Receiver = Spmc2ReceiverWrapper;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        let (tx, rxs) = enso_channel::fanout::spmc::channel::<u32, 2>(capacity);
        let [rx0, rx1] = rxs;
        // Drop the second receiver to unblock publisher for contract tests.
        drop(rx1);
        (tx, Spmc2ReceiverWrapper(rx0))
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

impl InduceUncommittedClaim for Spmc2Single {
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

struct Spmc2ReceiverWrapper(enso_channel::fanout::spmc::Receiver<u32>);

// ============================================================================
// MPMC (Multiple Publisher, Multiple Consumer)
// ============================================================================

struct Mpmc2;

impl h::FanoutAdapter<2> for Mpmc2 {
    type Sender = enso_channel::fanout::mpmc::Sender<u32, 2>;
    type Receiver = enso_channel::fanout::mpmc::Receiver<u32>;

    fn channel_fanout(capacity: usize) -> (Self::Sender, [Self::Receiver; 2]) {
        enso_channel::fanout::mpmc::channel::<u32, 2>(capacity)
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

/// Adapter for contract tests: uses only the first receiver.
struct Mpmc2Single;

impl Channel for Mpmc2Single {
    type Sender = enso_channel::fanout::mpmc::Sender<u32, 2>;
    type Receiver = Mpmc2ReceiverWrapper;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        let (tx, rxs) = enso_channel::fanout::mpmc::channel::<u32, 2>(capacity);
        let [rx0, rx1] = rxs;
        // Drop the second receiver to unblock publisher for contract tests.
        drop(rx1);
        (tx, Mpmc2ReceiverWrapper(rx0))
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

impl InduceUncommittedClaim for Mpmc2Single {
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

struct Mpmc2ReceiverWrapper(enso_channel::fanout::mpmc::Receiver<u32>);

// ============================================================================
// Contract tests via macro (using single-receiver adapters)
// ============================================================================

generate_contract_tests_prefixed!(
    fanout_spmc,
    Spmc2Single,
    [
        contract_fifo_order,
        contract_capacity_backpressure,
        contract_recv_empty,
        contract_publisher_drop_disconnects_consumers,
        contract_publisher_drop_disconnects_consumers_with_uncommitted_claims,
        contract_consumer_drop_disconnects_publishers,
    ]
);

generate_contract_tests_prefixed!(
    fanout_mpmc,
    Mpmc2Single,
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
// Fanout-specific tests
// ============================================================================

#[test]
fn fanout_spmc_each_receiver_sees_all_items() {
    h::each_receiver_sees_all_items::<2, Spmc2>();
}

#[test]
fn fanout_spmc_backpressured_by_slowest() {
    h::publisher_is_backpressured_by_slowest::<Spmc2>();
}

#[test]
fn fanout_spmc_publisher_drop_disconnects_consumers() {
    h::publisher_drop_disconnects_consumers::<Spmc2, 2>();
}

#[test]
fn fanout_mpmc_each_receiver_sees_all_items() {
    h::each_receiver_sees_all_items::<2, Mpmc2>();
}

#[test]
fn fanout_mpmc_backpressured_by_slowest() {
    h::publisher_is_backpressured_by_slowest::<Mpmc2>();
}

#[test]
fn fanout_mpmc_publisher_drop_disconnects_consumers() {
    h::publisher_drop_disconnects_consumers::<Mpmc2, 2>();
}

// ============================================================================
// Concurrency tests
// ============================================================================

impl harness::concurrency::CloneSender for Mpmc2Single {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

#[test]
fn fanout_spmc_concurrent_publisher_consumer() {
    harness::concurrency::contract_concurrent_spsc::<Spmc2Single>();
}

#[test]
fn fanout_mpmc_concurrent_publisher_consumer() {
    harness::concurrency::contract_concurrent_spsc::<Mpmc2Single>();
}

#[test]
fn fanout_mpmc_concurrent_multi_publisher() {
    harness::concurrency::contract_concurrent_mpsc::<Mpmc2Single>(4);
}
