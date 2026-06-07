#[macro_use]
mod harness;

use enso_channel::slot_recycler::ResetWithDefault;
use enso_channel::{ChanReadRefs, ChanReceiver, ChanSender, ChanWritePermit, ChanWritePermits};
use harness::broadcast as h;
use harness::shared::{Channel, RecvBatchU32, SendBatchU32};

struct BroadcastFanout2;

impl h::BroadcastAdapter<2> for BroadcastFanout2 {
    type Sender = enso_channel::fanout::Sender<2, u32>;
    type Receiver = enso_channel::fanout::Receiver<u32>;

    fn channel_broadcast(capacity: usize) -> (Self::Sender, [Self::Receiver; 2]) {
        enso_channel::fanout::channel::<2, u32>(capacity).unwrap()
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
}

struct BroadcastContractReceiver(enso_channel::fanout::Receiver<u32>);

/// Adapter for contract tests: uses only the first receiver.
struct BroadcastContractChan;

impl Channel for BroadcastContractChan {
    type Sender = enso_channel::fanout::Sender<2, u32>;
    type Receiver = BroadcastContractReceiver;

    type RecvBatch<'a>
        = enso_channel::fanout::ReadRefs<'a, u32>
    where
        Self::Receiver: 'a;

    type SendBatch<'a>
        = enso_channel::fanout::WritePermits<'a, 2, u32, ResetWithDefault>
    where
        Self::Sender: 'a;

    fn channel(capacity: usize) -> (Self::Sender, Self::Receiver) {
        let (tx, rxs) = enso_channel::fanout::channel::<2, u32>(capacity).unwrap();
        let [rx0, rx1] = rxs;
        drop(rx1);
        (tx, BroadcastContractReceiver(rx0))
    }

    fn try_send(
        sender: &mut Self::Sender,
        item: u32,
    ) -> Result<(), enso_channel::errors::TrySendError<u32>> {
        sender.try_send(item)
    }

    fn try_recv(receiver: &mut Self::Receiver) -> Result<u32, enso_channel::errors::TryRecvError> {
        let guard = receiver.0.try_recv()?;
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
            if let Some(item) = items.get(i) {
                permit.write(*item);
            } else {
                break;
            }
            i += 1;
        }
        Ok(n)
    }

    fn try_recv_at_most_batch<'a>(
        receiver: &'a mut Self::Receiver,
        limit: usize,
    ) -> Result<Self::RecvBatch<'a>, enso_channel::errors::TryRecvError> {
        receiver.0.try_recv_at_most(limit)
    }

    fn try_send_at_most_batch<'a>(
        sender: &'a mut Self::Sender,
        n: usize,
    ) -> Result<Self::SendBatch<'a>, enso_channel::errors::TrySendAtMostError> {
        sender.try_send_at_most(n, ResetWithDefault)
    }
}

impl<'a> RecvBatchU32 for enso_channel::fanout::ReadRefs<'a, u32> {
    fn to_vec(&mut self) -> Vec<u32> {
        self.iter().copied().collect()
    }

    fn finish(self) {
        drop(self)
    }
}

impl<'a> SendBatchU32 for enso_channel::fanout::WritePermits<'a, 2, u32, ResetWithDefault> {
    fn capacity(&self) -> usize {
        ChanWritePermits::total_reserved(self)
    }

    fn write_next(&mut self, item: u32) {
        self.next().unwrap().write(item);
    }

    fn finish(self) {
        enso_channel::fanout::WritePermits::commit(self)
    }
}

generate_contract_tests_prefixed!(
    broadcast,
    BroadcastContractChan,
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
