//! Generic contract test suites for channels.
//!
//! They are parameterized by channel type and can be applied to any implementation
//! that satisfies the required trait bounds.

use enso_channel::errors::{TryRecvAtMostError, TryRecvError, TrySendAtMostError, TrySendError};

use super::shared::Channel;

/// Test-only capability: induce a state where a publisher has successfully *claimed*
/// a range but failed to *publish/commit* it.
///
/// This is used to validate shutdown/disconnect behavior in the presence of
/// mismatched claim vs publish progress (e.g., panics in user-provided factories).
pub trait InduceUncommittedClaim: Channel {
    /// Advances internal claim state without committing/publishing the claimed range.
    fn induce_uncommitted_claim(sender: &mut Self::Sender, n: usize);
}

/// Contract: FIFO ordering is preserved.
///
/// Send N items, verify recv order matches send order exactly.
pub fn contract_fifo_order<C: Channel>() {
    let (mut tx, mut rx) = C::channel(8);

    for i in 0..5 {
        C::try_send(&mut tx, i).expect("send should succeed");
    }

    for i in 0..5 {
        let v = C::try_recv(&mut rx).expect("recv should succeed");
        assert_eq!(v, i, "FIFO order violated: expected {i}, got {v}");
    }
}

/// Contract: Capacity backpressure.
///
/// Fill buffer to capacity, assert `InsufficientCapacity` on next send.
pub fn contract_capacity_backpressure<C: Channel>() {
    let capacity = 4;
    let (mut tx, mut rx) = C::channel(capacity);

    // Fill to capacity
    for i in 0..capacity as u32 {
        C::try_send(&mut tx, i).expect("send within capacity should succeed");
    }

    // Next send should fail with InsufficientCapacity
    match C::try_send(&mut tx, 99) {
        Err(TrySendError::InsufficientCapacity { missing }) => {
            assert_eq!(missing, 1, "should be missing exactly 1 slot");
        }
        Err(e) => panic!("expected InsufficientCapacity, got {e:?}"),
        Ok(()) => panic!("expected InsufficientCapacity, but send succeeded"),
    }

    // Consume one item to free a slot
    let _ = C::try_recv(&mut rx).expect("recv should succeed");

    // Now send should succeed
    C::try_send(&mut tx, 99).expect("send after freeing slot should succeed");

    let items = vec![0, 2];
    match C::try_send_many(&mut tx, &items) {
        Ok(_) => panic!("expected InsufficientCapacity on batch send, but succeeded"),
        Err(TrySendError::Disconnected) => {
            panic!("expected InsufficientCapacity on batch send, got Disconnected");
        }
        Err(TrySendError::InsufficientCapacity { missing }) => {
            assert_eq!(
                missing,
                items.len(),
                "should be missing exactly {} slots",
                items.len()
            );
        }
    }

    match C::try_send_at_most(&mut tx, &items) {
        Err(TrySendAtMostError::Full) => {}
        _ => panic!("expected Full on batch send_at_most"),
    }
}

/// Contract: Empty channel returns InsufficientItems.
///
/// Attempting to receive from an empty channel should return `InsufficientItems`.
pub fn contract_recv_empty<C: Channel>() {
    let (_tx, mut rx) = C::channel(8);

    match C::try_recv(&mut rx) {
        Err(TryRecvError::InsufficientItems { missing }) => {
            assert_eq!(missing, 1);
        }
        Err(e) => panic!("expected InsufficientItems, got {e:?}"),
        Ok(v) => panic!("expected InsufficientItems, got Ok({v})"),
    }

    match C::try_recv_many(&mut rx, 3) {
        Err(TryRecvError::InsufficientItems { missing }) => {
            assert_eq!(missing, 3);
        }
        Err(e) => panic!("expected InsufficientItems on batch recv, got {e:?}"),
        Ok(v) => panic!("expected InsufficientItems on batch recv, got Ok({v:?})"),
    }

    match C::try_recv_at_most(&mut rx, 3) {
        Err(TryRecvAtMostError::Empty) => {}
        Err(e) => panic!("expected InsufficientItems on batch recv_at_most, got {e:?}"),
        Ok(v) => panic!("expected InsufficientItems on batch recv_at_most, got Ok({v:?})"),
    }
}

/// Contract: Dropping the last publisher disconnects consumers.
///
/// After the last sender is dropped, consumer eventually returns `Disconnected`
/// once it has consumed all committed sequences <= shutdown boundary.
pub fn contract_publisher_drop_disconnects_consumers<C: Channel>() {
    let (mut tx, mut rx) = C::channel(8);

    let total_to_send = 8;

    // Send some items
    for i in 0..total_to_send {
        C::try_send(&mut tx, i).expect("send should succeed");
    }

    // Drop the last sender
    drop(tx);

    let poll_size = 5; // There should be one failed batch recv attempt
    let first_poll = C::try_recv_many(&mut rx, poll_size).expect("first poll should succeed");
    assert_eq!(first_poll.len(), poll_size);
    let second_poll = C::try_recv_many(&mut rx, poll_size);
    assert!(matches!(
        second_poll,
        Err(TryRecvError::InsufficientItems { missing: 2 })
    ));

    // Drain remaining items should succeed
    let remaining_items =
        C::try_recv_at_most(&mut rx, poll_size).expect("draining recv should succeed");
    assert_eq!(remaining_items.len(), total_to_send as usize - poll_size);

    // Following recv should return Disconnected
    match C::try_recv(&mut rx) {
        Err(TryRecvError::Disconnected) => {}
        Err(e) => panic!("expected Disconnected after publisher drop, got {e:?}"),
        Ok(v) => panic!("expected Disconnected, got Ok({v})"),
    }

    match C::try_recv_many(&mut rx, poll_size) {
        Err(TryRecvError::Disconnected) => {}
        Err(e) => {
            panic!("expected Disconnected after publisher drop on batch recv, got {e:?}")
        }
        Ok(v) => panic!("expected Disconnected on batch recv, got Ok({v:?})"),
    }

    match C::try_recv_at_most(&mut rx, poll_size) {
        Err(TryRecvAtMostError::Disconnected) => {}
        Err(e) => {
            panic!("expected Disconnected after publisher drop on batch recv_at_most, got {e:?}")
        }
        Ok(v) => panic!("expected Disconnected on batch recv_at_most, got Ok({v:?})"),
    }
}

/// Contract: Publisher drop disconnects consumers even if there are uncommitted claims.
///
/// This guards against shutdown boundaries being derived from claim cursors.
/// The receiver should not wait for sequences that were never published.
pub fn contract_publisher_drop_disconnects_consumers_with_uncommitted_claims<C>()
where
    C: Channel + InduceUncommittedClaim,
{
    let (mut tx, mut rx) = C::channel(8);

    // Create a claim/publish mismatch: claim succeeds, publish does not.
    C::induce_uncommitted_claim(&mut tx, 4);

    // No committed items should be observable.
    match C::try_recv(&mut rx) {
        Err(TryRecvError::InsufficientItems { missing: 1 }) => {}
        Err(TryRecvError::Disconnected) => {
            panic!("expected InsufficientItems before dropping sender")
        }
        Err(e) => panic!("expected InsufficientItems, got {e:?}"),
        Ok(v) => panic!("expected InsufficientItems, got Ok({v})"),
    }

    // Dropping the last sender must disconnect receivers without hanging.
    drop(tx);

    match C::try_recv(&mut rx) {
        Err(TryRecvError::Disconnected) => {}
        other => panic!("expected Disconnected after publisher drop, got {other:?}"),
    }

    match C::try_recv_many(&mut rx, 1) {
        Err(TryRecvError::Disconnected) => {}
        other => panic!("expected Disconnected on recv_many after publisher drop, got {other:?}"),
    }

    match C::try_recv_at_most(&mut rx, 1) {
        Err(TryRecvAtMostError::Disconnected) => {}
        other => {
            panic!("expected Disconnected on recv_at_most after publisher drop, got {other:?}")
        }
    }
}

/// Contract: Dropping the last consumer disconnects publishers.
///
/// After the last receiver is dropped, publisher returns `TrySendError::Disconnected`.
///
/// Note: some publisher implementations cache consumer-derived availability bounds
/// (e.g. `max_available`). This contract intentionally performs a successful warm-up
/// send before dropping the receiver to ensure cached fast-paths still observe
/// disconnect immediately.
pub fn contract_consumer_drop_disconnects_publishers<C: Channel>() {
    let (mut tx, rx) = C::channel(8);

    // Warm any internal publisher availability caches.
    C::try_send(&mut tx, 0).expect("warm-up send should succeed");

    // Drop the last receiver
    drop(rx);

    // Following Send should return Disconnected
    match C::try_send(&mut tx, 1) {
        Err(TrySendError::Disconnected) => {}
        Err(e) => panic!("expected Disconnected after consumer drop, got {e:?}"),
        Ok(()) => panic!("expected Disconnected, but send succeeded"),
    }

    match C::try_send_many(&mut tx, &[1, 2, 3]) {
        Err(TrySendError::Disconnected) => {}
        Err(e) => panic!("expected Disconnected after consumer drop on batch send, got {e:?}"),
        Ok(()) => panic!("expected Disconnected on batch send, but succeeded"),
    }

    match C::try_send_at_most(&mut tx, &[1, 2, 3]) {
        Err(TrySendAtMostError::Disconnected) => {}
        Err(e) => {
            panic!("expected Disconnected after consumer drop on batch send_at_most, got {e:?}")
        }
        Ok(_) => panic!("expected Disconnected on batch send_at_most, but succeeded"),
    }
}

/// Contract: Claim exceeding capacity fails immediately.
///
/// Attempting to send more items than capacity in a single batch should fail.
pub fn contract_claim_exceeds_capacity<C: Channel>() {
    let capacity = 4;
    let (mut tx, mut rx) = C::channel(capacity);

    // Fill to capacity first
    for i in 0..capacity as u32 {
        C::try_send(&mut tx, i).expect("send within capacity should succeed");
    }

    // Attempting to send when full should fail
    match C::try_send(&mut tx, 99) {
        Err(TrySendError::InsufficientCapacity { missing }) => {
            assert_eq!(missing, 1, "should be missing exactly 1 slot");
        }
        Err(e) => panic!("expected InsufficientCapacity, got {e:?}"),
        Ok(()) => panic!("expected InsufficientCapacity, but send succeeded"),
    }

    // Drain the channel
    for _ in 0..capacity {
        let _ = C::try_recv(&mut rx).expect("recv should succeed");
    }

    let items_to_send = vec![0; capacity + 1];
    // Attempting to send more than capacity should fail
    match C::try_send_many(&mut tx, &items_to_send) {
        Err(TrySendError::InsufficientCapacity { missing }) => {
            assert_eq!(missing, 1);
        }
        Err(e) => panic!("expected InsufficientCapacity on batch claim, got {e:?}"),
        Ok(_) => panic!("expected InsufficientCapacity on batch claim, but succeeded"),
    }

    let actual_sent =
        C::try_send_at_most(&mut tx, &items_to_send).expect("send should fit the capacity");
    assert_eq!(actual_sent, capacity, "should send up to capacity");

    // Attempting to recv more than capacity should fail
    match C::try_recv_many(&mut rx, capacity + 1) {
        Err(TryRecvError::InsufficientItems { missing }) => {
            assert_eq!(missing, 1);
        }
        Err(e) => panic!("expected InsufficientItems on batch claim, got {e:?}"),
        Ok(_) => panic!("expected InsufficientItems on batch claim, but succeeded"),
    }

    let actual_recv = C::try_recv_at_most(&mut rx, capacity + 1)
        .expect("recv should fit the capacity")
        .len();
    assert_eq!(actual_recv, capacity, "should recv up to capacity");
}
