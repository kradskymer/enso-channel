#[macro_use]
mod harness;

use enso_channel::slot_recycler::ResetWithDefault;
use enso_channel::{ChanReadRefs, ChanReceiver, ChanSender, ChanWritePermit, ChanWritePermits};
use harness::mpsc as h;
use harness::shared::{Channel, RecvBatchU32, SendBatchU32};

struct MpscChan;

impl Channel for MpscChan {
    type Sender = enso_channel::mpsc::Sender<u32>;
    type Receiver = enso_channel::mpsc::Receiver<u32>;

    type RecvBatch<'a>
        = enso_channel::mpsc::ReadRefs<'a, u32>
    where
        Self::Receiver: 'a;

    type SendBatch<'a>
        = enso_channel::mpsc::WritePermits<'a, u32, ResetWithDefault>
    where
        Self::Sender: 'a;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        enso_channel::mpsc::channel::<u32>(capacity).unwrap()
    }

    fn try_send(
        sender: &mut Self::Sender,
        item: u32,
    ) -> Result<(), enso_channel::errors::TrySendError<u32>> {
        sender.try_send(item)
    }

    fn try_recv(receiver: &mut Self::Receiver) -> Result<u32, enso_channel::errors::TryRecvError> {
        let guard = receiver.try_recv()?;
        Ok(*guard)
    }

    fn try_send_at_most(
        sender: &mut Self::Sender,
        items: &[u32],
    ) -> Result<usize, enso_channel::errors::TrySendAtMostError> {
        let mut batch = sender.try_send_at_most(items.len(), ResetWithDefault)?;
        let n = batch.capacity();
        let mut i = 0;
        while let Some(permit) = batch.next() {
            permit.write(items[i]);
            i += 1;
        }
        Ok(n)
    }

    fn try_send_at_most_batch<'a>(
        sender: &'a mut Self::Sender,
        n: usize,
    ) -> Result<Self::SendBatch<'a>, enso_channel::errors::TrySendAtMostError> {
        sender.try_send_at_most(n, ResetWithDefault)
    }

    fn try_recv_at_most_batch<'a>(
        receiver: &'a mut Self::Receiver,
        limit: usize,
    ) -> Result<Self::RecvBatch<'a>, enso_channel::errors::TryRecvError> {
        receiver.try_recv_at_most(limit)
    }
}

impl h::MpscChannel for MpscChan {}

impl h::CloneSender for MpscChan {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender {
        sender.clone()
    }
}

impl<'a> RecvBatchU32 for enso_channel::mpsc::ReadRefs<'a, u32> {
    fn to_vec(&mut self) -> Vec<u32> {
        self.iter().copied().collect()
    }

    fn finish(self) {
        drop(self)
    }
}

impl<'a> SendBatchU32 for enso_channel::mpsc::WritePermits<'a, u32, ResetWithDefault> {
    fn capacity(&self) -> usize {
        self.total_reserved()
    }

    fn write_next(&mut self, item: u32) {
        self.next().unwrap().write(item);
    }

    fn finish(self) {
        enso_channel::mpsc::WritePermits::commit(self)
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
        contract_send_batch_drop_fills_with_factory_and_commits,
        contract_recv_empty,
        contract_recv_many_batch_iter_yields_fifo,
        contract_recv_batch_holds_capacity_until_commit,
        contract_recv_batch_finish_commits_immediately,
        contract_recv_at_most_batch_is_bounded,
        contract_recv_batch_drop_commits_without_iteration,
        contract_publisher_drop_disconnects_consumers,
        contract_consumer_drop_disconnects_publishers,
    ]
);

// ============================================================================
// MPSC-specific tests
// ============================================================================

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
