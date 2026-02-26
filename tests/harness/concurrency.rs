//! Concurrency test utilities and contracts for channels.
//!
//! These tests verify safety invariants under concurrent access:
//! - No data races
//! - No torn reads (partial/corrupted data)
//! - Publish-before-read ordering (memory model correctness)
//! - Work distribution correctness for multi-consumer patterns

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use enso_channel::errors::{TryRecvError, TrySendError};

use super::shared::Channel;

// ============================================================================
// Test configuration
// ============================================================================

#[cfg(miri)]
const TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(not(miri))]
const TIMEOUT: Duration = Duration::from_secs(10);

const CHANNEL_CAPACITY: usize = 64;

const SPSC_ITEMS: u32 = 1000;
const MPSC_ITEMS_PER_PUBLISHER: u32 = 500;
const WORK_STEALING_ITEMS: u32 = 1000;
const MPMC_WORK_STEALING_ITEMS_PER_PUBLISHER: u32 = 300;

/// Trait for channels with thread-safe sender.
pub trait SendChannel: Channel
where
    Self::Sender: Send,
{
}

impl<C: Channel> SendChannel for C where C::Sender: Send {}

/// Trait for channels with thread-safe receiver.
pub trait RecvChannel: Channel
where
    Self::Receiver: Send,
{
}

impl<C: Channel> RecvChannel for C where C::Receiver: Send {}

/// Trait for channels with cloneable sender (multi-publisher).
pub trait CloneSender: Channel {
    fn clone_sender(sender: &Self::Sender) -> Self::Sender;
}

/// Trait for channels with cloneable receiver (multi-consumer).
pub trait CloneReceiver: Channel {
    fn clone_receiver(receiver: &Self::Receiver) -> Self::Receiver;
}

// ============================================================================
// Single Publisher, Single Consumer Concurrency
// ============================================================================

/// Contract: Concurrent SPSC maintains FIFO order and data integrity.
///
/// One publisher thread sends N items, one consumer thread receives them.
/// Verifies all items are received in order with correct values.
pub fn contract_concurrent_spsc<C>()
where
    C: Channel + 'static,
    C::Sender: Send,
    C::Receiver: Send,
{
    let (mut tx, mut rx) = C::channel(CHANNEL_CAPACITY);

    thread::scope(|s| {
        // Publisher thread
        let publisher = s.spawn(move || {
            for i in 0..SPSC_ITEMS {
                let start = Instant::now();
                loop {
                    match C::try_send(&mut tx, i) {
                        Ok(()) => break,
                        Err(TrySendError::InsufficientCapacity { .. }) => {
                            if start.elapsed() > TIMEOUT {
                                panic!("publisher timed out at item {i}");
                            }
                            thread::yield_now();
                        }
                        Err(TrySendError::Disconnected) => {
                            panic!("publisher disconnected at item {i}");
                        }
                    }
                }
            }
        });

        // Consumer thread
        let consumer = s.spawn(move || {
            let mut received = Vec::with_capacity(SPSC_ITEMS as usize);
            let mut last_progress = Instant::now();
            loop {
                match C::try_recv(&mut rx) {
                    Ok(v) => {
                        received.push(v);
                        last_progress = Instant::now();
                    }
                    Err(TryRecvError::InsufficientItems { .. }) => {
                        if last_progress.elapsed() > TIMEOUT {
                            panic!("consumer timed out. received: {}", received.len());
                        }
                        thread::yield_now();
                    }
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            received
        });

        publisher.join().expect("publisher panicked");
        let received = consumer.join().expect("consumer panicked");

        // Verify all items received in order
        assert_eq!(received.len(), SPSC_ITEMS as usize, "item count mismatch");
        for (i, &v) in received.iter().enumerate() {
            assert_eq!(v, i as u32, "FIFO order violated at index {i}");
        }
    });
}

// ============================================================================
// Multi Publisher, Single Consumer Concurrency
// ============================================================================

/// Contract: Concurrent MPSC delivers all items without loss.
///
/// N publisher threads each send M items (tagged with publisher ID).
/// One consumer thread receives all items.
/// Verifies no items are lost and each publisher's items are in order.
pub fn contract_concurrent_mpsc<C>(num_publishers: usize)
where
    C: Channel + CloneSender + 'static,
    C::Sender: Send,
    C::Receiver: Send,
{
    let (tx, mut rx) = C::channel(CHANNEL_CAPACITY);

    // Signal for publishers to indicate they're done
    let publishers_done = Arc::new(AtomicBool::new(false));

    thread::scope(|s| {
        // Spawn consumer thread first (so it can drain items as publishers send)
        let publishers_done_clone = publishers_done.clone();
        let consumer = s.spawn(move || {
            let mut received: Vec<Vec<u32>> = vec![Vec::new(); num_publishers];
            let mut last_progress = Instant::now();
            loop {
                match C::try_recv(&mut rx) {
                    Ok(v) => {
                        let publisher_id = (v >> 16) as usize;
                        let item = v & 0xFFFF;
                        received[publisher_id].push(item);
                        last_progress = Instant::now();
                    }
                    Err(TryRecvError::InsufficientItems { .. }) => {
                        if last_progress.elapsed() > TIMEOUT {
                            panic!("consumer timed out");
                        }
                        // If publishers are done and no items, we might be waiting for stragglers
                        if publishers_done_clone.load(Ordering::Acquire) {
                            // Final drain attempt
                            thread::yield_now();
                        } else {
                            thread::yield_now();
                        }
                    }
                    Err(TryRecvError::Disconnected) => break,
                }
            }
            received
        });

        // Spawn publisher threads
        let publishers: Vec<_> = (0..num_publishers)
            .map(|publisher_id| {
                let mut tx = C::clone_sender(&tx);
                s.spawn(move || {
                    for i in 0..MPSC_ITEMS_PER_PUBLISHER {
                        // Encode publisher_id in high bits, item index in low bits
                        let value = ((publisher_id as u32) << 16) | i;
                        let start = Instant::now();
                        loop {
                            match C::try_send(&mut tx, value) {
                                Ok(()) => break,
                                Err(TrySendError::InsufficientCapacity { .. }) => {
                                    if start.elapsed() > TIMEOUT {
                                        panic!("publisher {publisher_id} timed out at item {i}");
                                    }
                                    thread::yield_now();
                                }
                                Err(TrySendError::Disconnected) => {
                                    panic!("publisher {publisher_id} disconnected at item {i}");
                                }
                            }
                        }
                    }
                })
            })
            .collect();

        // Wait for all publishers to finish, then terminate
        for p in publishers {
            p.join().expect("publisher panicked");
        }
        publishers_done.store(true, Ordering::Release);
        drop(tx);

        // Wait for consumer to finish and get results
        let received = consumer.join().expect("consumer panicked");

        // Verify each publisher's items are in order
        for (publisher_id, items) in received.iter().enumerate() {
            assert_eq!(
                items.len(),
                MPSC_ITEMS_PER_PUBLISHER as usize,
                "publisher {publisher_id} item count mismatch"
            );
            for (i, &v) in items.iter().enumerate() {
                assert_eq!(
                    v, i as u32,
                    "publisher {publisher_id} order violated at index {i}"
                );
            }
        }
    });
}

// ============================================================================
// Work-Stealing (Queue) Multi-Consumer Concurrency
// ============================================================================

/// Contract: Work-stealing delivers each item to exactly one consumer.
///
/// One publisher sends N items. M consumers compete to receive.
/// Verifies each item is received exactly once (no duplicates, no loss).
pub fn contract_concurrent_work_stealing<C>(num_consumers: usize)
where
    C: Channel + CloneReceiver + 'static,
    C::Sender: Send,
    C::Receiver: Send,
{
    let (mut tx, rx) = C::channel(CHANNEL_CAPACITY);
    let done = Arc::new(AtomicBool::new(false));

    thread::scope(|s| {
        // Spawn consumer threads
        let consumers: Vec<_> = (0..num_consumers)
            .map(|_| {
                let mut rx = C::clone_receiver(&rx);
                let done = done.clone();
                s.spawn(move || {
                    let mut received = Vec::new();
                    let mut last_progress = Instant::now();
                    loop {
                        match C::try_recv(&mut rx) {
                            Ok(v) => {
                                received.push(v);
                                last_progress = Instant::now();
                            }
                            Err(TryRecvError::InsufficientItems { .. }) => {
                                if last_progress.elapsed() > TIMEOUT {
                                    panic!("consumer timed out");
                                }
                                if done.load(Ordering::Acquire) {
                                    // Double-check after seeing done flag
                                    match C::try_recv(&mut rx) {
                                        Ok(v) => received.push(v),
                                        Err(_) => break,
                                    }
                                } else {
                                    thread::yield_now();
                                }
                            }
                            Err(TryRecvError::Disconnected) => break,
                        }
                    }
                    received
                })
            })
            .collect();

        // publisher sends all items
        for i in 0..WORK_STEALING_ITEMS {
            let start = Instant::now();
            loop {
                match C::try_send(&mut tx, i) {
                    Ok(()) => break,
                    Err(TrySendError::InsufficientCapacity { .. }) => {
                        if start.elapsed() > TIMEOUT {
                            panic!("publisher timed out at item {i}");
                        }
                        thread::yield_now();
                    }
                    Err(TrySendError::Disconnected) => {
                        panic!("publisher disconnected at item {i}");
                    }
                }
            }
        }
        drop(tx);
        done.store(true, Ordering::Release);

        // Collect results from all consumers
        let mut all_received: HashSet<u32> = HashSet::new();
        let mut total_count = 0;
        for consumer in consumers {
            let items = consumer.join().expect("consumer panicked");
            total_count += items.len();
            for item in items {
                assert!(
                    all_received.insert(item),
                    "duplicate item {item} received by multiple consumers"
                );
            }
        }

        // Verify all items received exactly once
        assert_eq!(
            total_count, WORK_STEALING_ITEMS as usize,
            "total item count mismatch"
        );
        for i in 0..WORK_STEALING_ITEMS {
            assert!(all_received.contains(&i), "item {i} was lost");
        }
    });
}

// ============================================================================
// Full MPMC Concurrency (Multi-publisher, Multi-Consumer Work-Stealing)
// ============================================================================

/// Contract: Full MPMC with work-stealing delivers all items exactly once.
///
/// N publishers each send M items. K consumers compete to receive.
/// Verifies each item is received exactly once.
pub fn contract_concurrent_mpmc_work_stealing<C>(num_publishers: usize, num_consumers: usize)
where
    C: Channel + CloneSender + CloneReceiver + 'static,
    C::Sender: Send,
    C::Receiver: Send,
{
    let (tx, rx) = C::channel(CHANNEL_CAPACITY);
    let done = Arc::new(AtomicBool::new(false));

    thread::scope(|s| {
        // Spawn consumer threads first
        let consumers: Vec<_> = (0..num_consumers)
            .map(|_| {
                let mut rx = C::clone_receiver(&rx);
                let done = done.clone();
                s.spawn(move || {
                    let mut received = Vec::new();
                    let mut last_progress = Instant::now();
                    loop {
                        match C::try_recv(&mut rx) {
                            Ok(v) => {
                                received.push(v);
                                last_progress = Instant::now();
                            }
                            Err(TryRecvError::InsufficientItems { .. }) => {
                                if last_progress.elapsed() > TIMEOUT {
                                    panic!("consumer timed out");
                                }
                                if done.load(Ordering::Acquire) {
                                    match C::try_recv(&mut rx) {
                                        Ok(v) => received.push(v),
                                        Err(_) => break,
                                    }
                                } else {
                                    thread::yield_now();
                                }
                            }
                            Err(TryRecvError::Disconnected) => break,
                        }
                    }
                    received
                })
            })
            .collect();

        // Spawn publisher threads
        let publishers: Vec<_> = (0..num_publishers)
            .map(|publisher_id| {
                let mut tx = C::clone_sender(&tx);
                s.spawn(move || {
                    for i in 0..MPMC_WORK_STEALING_ITEMS_PER_PUBLISHER {
                        let value = ((publisher_id as u32) << 16) | i;
                        let start = Instant::now();
                        loop {
                            match C::try_send(&mut tx, value) {
                                Ok(()) => break,
                                Err(TrySendError::InsufficientCapacity { .. }) => {
                                    if start.elapsed() > TIMEOUT {
                                        panic!("publisher {publisher_id} timed out at item {i}");
                                    }
                                    thread::yield_now();
                                }
                                Err(TrySendError::Disconnected) => {
                                    panic!("publisher {publisher_id} disconnected at item {i}");
                                }
                            }
                        }
                    }
                })
            })
            .collect();

        // Wait for publishers, then signal done
        for p in publishers {
            p.join().expect("publisher panicked");
        }
        drop(tx);
        done.store(true, Ordering::Release);

        // Collect and verify
        let mut all_received: HashSet<u32> = HashSet::new();
        for consumer in consumers {
            let items = consumer.join().expect("consumer panicked");
            for item in items {
                assert!(all_received.insert(item), "duplicate item {item} received");
            }
        }

        let expected_total = num_publishers * MPMC_WORK_STEALING_ITEMS_PER_PUBLISHER as usize;
        assert_eq!(all_received.len(), expected_total, "item count mismatch");
    });
}
