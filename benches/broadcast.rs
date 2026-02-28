//! Broadcast channel benchmarks for enso_channel.
//!
//! Tests the fixed broadcast pattern where each item is delivered
//! to ALL consumers, with producer gating by the slowest consumer.
//!
//! No direct competitor exists for lossless fixed broadcast, so we measure absolute throughput.

use std::sync::Barrier;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

mod bench_config;

// Consumer and configuration constants

const BATCH_SIZES: [usize; 2] = [16, 64];
const BROADCAST_P1_CONSUMER_COUNTS: [usize; 2] = [2, 4];
const BROADCAST_PN_CONFIGS: [(usize, usize); 3] = [(2, 2), (2, 4), (4, 2)];

// Helper to register single-producer single + batch variants for a const consumer count
fn register_broadcast_p1_variants<const N: usize>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    capacity: usize,
    message_count: usize,
    batch_sizes: &[usize],
) {
    group.throughput(Throughput::Elements((message_count * N) as u64));
    let scenario_base = format!("cap_{capacity}/c{N}");

    // single
    group.bench_with_input(
        BenchmarkId::new("single", &scenario_base),
        &capacity,
        |b, &cap| {
            b.iter(|| run_broadcast_p1::<N>(cap, message_count));
        },
    );

    // batches
    for &bs in batch_sizes {
        let scenario_batch = format!("{scenario_base}/b{bs}");
        group.bench_with_input(
            BenchmarkId::new("batch", &scenario_batch),
            &(capacity, bs),
            |b, &(cap, bs)| {
                b.iter(|| run_broadcast_p1_batch::<N>(cap, message_count, bs));
            },
        );
    }
}

// Helper to register multi-producer single + batch variants for a const consumer count
fn register_broadcast_pn_variants<const N: usize>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    capacity: usize,
    producers: usize,
    messages_per_producer: usize,
    batch_sizes: &[usize],
) {
    let total_messages = producers * messages_per_producer;
    group.throughput(Throughput::Elements((total_messages * N) as u64));

    let scenario_base = format!("cap_{capacity}/p{producers}_c{N}");

    // single
    group.bench_with_input(
        BenchmarkId::new("single", &scenario_base),
        &(capacity, producers, messages_per_producer),
        |b, &(cap, prod, mpp)| {
            b.iter(|| run_broadcast_pn::<N>(cap, prod, mpp));
        },
    );

    // batches
    for &bs in batch_sizes {
        let scenario_batch = format!("{scenario_base}/b{bs}");
        group.bench_with_input(
            BenchmarkId::new("batch", &scenario_batch),
            &(capacity, producers, messages_per_producer, bs),
            |b, &(cap, prod, mpp, bs)| {
                b.iter(|| run_broadcast_pn_batch::<N>(cap, prod, mpp, bs));
            },
        );
    }
}

// =============================================================================
// Broadcast channel wrapper (const generic N)
// =============================================================================

trait BroadcastSender<const N: usize>: Send {
    fn send(&mut self, value: u64);
}

trait BroadcastReceiver: Send {
    fn recv(&mut self) -> u64;
}

// -----------------------------------------------------------------------------
// enso_channel::broadcast
// -----------------------------------------------------------------------------

impl<const N: usize> BroadcastSender<N> for enso_channel::broadcast::Sender<u64, N> {
    fn send(&mut self, value: u64) {
        let backoff = crossbeam_utils::Backoff::new();
        loop {
            match self.try_send(std::hint::black_box(value)) {
                Ok(()) => return,
                Err(enso_channel::errors::TrySendError::InsufficientCapacity { .. }) => {
                    backoff.spin();
                }
                Err(enso_channel::errors::TrySendError::Disconnected) => {
                    panic!("channel disconnected");
                }
            }
        }
    }
}

impl BroadcastReceiver for enso_channel::broadcast::Receiver<u64> {
    fn recv(&mut self) -> u64 {
        let backoff = crossbeam_utils::Backoff::new();
        loop {
            match self.try_recv() {
                Ok(guard) => return *guard,
                Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                    backoff.spin();
                }
                Err(enso_channel::errors::TryRecvError::Disconnected) => {
                    return u64::MAX; // Signal termination
                }
            }
        }
    }
}

// =============================================================================
// Benchmark scenarios
// =============================================================================

/// Broadcast with N consumers: producer sends, all consumers receive every item.
fn run_broadcast_p1<const N: usize>(capacity: usize, message_count: usize) {
    let (mut tx, rxs): (enso_channel::broadcast::Sender<u64, N>, _) =
        enso_channel::broadcast::channel(capacity);

    let barrier = Barrier::new(N + 1);
    let barrier = &barrier;

    crossbeam_utils::thread::scope(|s| {
        // Consumer threads
        for mut rx in rxs {
            s.spawn(move |_| {
                barrier.wait();
                for _ in 0..message_count {
                    let v = rx.recv();
                    if v == u64::MAX {
                        break;
                    }
                    std::hint::black_box(v);
                }
            });
        }

        // Producer thread (main)
        barrier.wait();
        for i in 0..message_count as u64 {
            tx.send(std::hint::black_box(i));
        }
        drop(tx);
    })
    .expect("threads should join");
}

/// Broadcast with N consumers and P producers.
fn run_broadcast_pn<const N: usize>(
    capacity: usize,
    producer_count: usize,
    messages_per_producer: usize,
) {
    let (tx, rxs): (enso_channel::broadcast::Sender<u64, N>, _) =
        enso_channel::broadcast::channel(capacity);

    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(N + producer_count);
    let barrier = &barrier;

    crossbeam_utils::thread::scope(|s| {
        // Consumer threads
        for mut rx in rxs {
            s.spawn(move |_| {
                barrier.wait();
                let backoff = crossbeam_utils::Backoff::new();
                for _ in 0..total_messages {
                    backoff.reset();
                    loop {
                        match rx.try_recv() {
                            Ok(guard) => {
                                std::hint::black_box(*guard);
                                break;
                            }
                            Err(enso_channel::errors::TryRecvError::InsufficientItems {
                                ..
                            }) => {
                                backoff.spin();
                            }
                            Err(enso_channel::errors::TryRecvError::Disconnected) => {
                                return;
                            }
                        }
                    }
                }
            });
        }

        // Producer threads
        for _ in 0..producer_count {
            let mut tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                let backoff = crossbeam_utils::Backoff::new();
                for i in 0..messages_per_producer as u64 {
                    backoff.reset();
                    loop {
                        match tx.try_send(std::hint::black_box(i)) {
                            Ok(()) => break,
                            Err(enso_channel::errors::TrySendError::InsufficientCapacity {
                                ..
                            }) => {
                                backoff.spin();
                            }
                            Err(enso_channel::errors::TrySendError::Disconnected) => {
                                panic!("channel disconnected");
                            }
                        }
                    }
                }
            });
        }
    })
    .expect("threads should join");
}

// =============================================================================
// Batch benchmark scenarios - enso_channel only (unique batch API advantage)
// =============================================================================

/// Batch broadcast with N consumers: producer batch-sends, all consumers batch-receive.
fn run_broadcast_p1_batch<const N: usize>(
    capacity: usize,
    message_count: usize,
    batch_size: usize,
) {
    let (mut tx, rxs): (enso_channel::broadcast::Sender<u64, N>, _) =
        enso_channel::broadcast::channel(capacity);

    let barrier = Barrier::new(N + 1);
    let barrier = &barrier;

    crossbeam_utils::thread::scope(|s| {
        // Consumer threads - batch receive
        for mut rx in rxs {
            s.spawn(move |_| {
                barrier.wait();
                let mut received = 0;
                let backoff = crossbeam_utils::Backoff::new();
                while received < message_count {
                    let to_recv = batch_size.min(message_count - received);
                    backoff.reset();
                    loop {
                        match rx.try_recv_many(to_recv) {
                            Ok(iter) => {
                                for v in iter {
                                    std::hint::black_box(*v);
                                }
                                received += to_recv;
                                break;
                            }
                            Err(enso_channel::errors::TryRecvError::InsufficientItems {
                                ..
                            }) => {
                                backoff.spin();
                            }
                            Err(enso_channel::errors::TryRecvError::Disconnected) => {
                                return;
                            }
                        }
                    }
                }
            });
        }

        // Producer thread - batch send
        barrier.wait();
        let mut sent = 0usize;
        let backoff = crossbeam_utils::Backoff::new();
        while sent < message_count {
            let to_send = batch_size.min(message_count - sent);
            backoff.reset();
            loop {
                match tx.try_send_many(to_send, || std::hint::black_box(0u64)) {
                    Ok(mut batch) => {
                        for i in 0..to_send {
                            batch.write_next(std::hint::black_box((sent + i) as u64));
                        }
                        batch.finish();
                        sent += to_send;
                        break;
                    }
                    Err(enso_channel::errors::TrySendError::InsufficientCapacity { .. }) => {
                        backoff.spin();
                    }
                    Err(enso_channel::errors::TrySendError::Disconnected) => {
                        panic!("channel disconnected");
                    }
                }
            }
        }
        drop(tx);
    })
    .expect("threads should join");
}

/// Batch broadcast with N consumers and P producers.
fn run_broadcast_pn_batch<const N: usize>(
    capacity: usize,
    producer_count: usize,
    messages_per_producer: usize,
    batch_size: usize,
) {
    let (tx, rxs): (enso_channel::broadcast::Sender<u64, N>, _) =
        enso_channel::broadcast::channel(capacity);

    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(N + producer_count);
    let barrier = &barrier;

    crossbeam_utils::thread::scope(|s| {
        // Consumer threads - batch receive
        for mut rx in rxs {
            s.spawn(move |_| {
                barrier.wait();
                let mut received = 0;
                let backoff = crossbeam_utils::Backoff::new();
                while received < total_messages {
                    let to_recv = batch_size.min(total_messages - received);
                    backoff.reset();
                    loop {
                        match rx.try_recv_many(to_recv) {
                            Ok(iter) => {
                                for v in iter {
                                    std::hint::black_box(*v);
                                }
                                received += to_recv;
                                break;
                            }
                            Err(enso_channel::errors::TryRecvError::InsufficientItems {
                                ..
                            }) => {
                                backoff.spin();
                            }
                            Err(enso_channel::errors::TryRecvError::Disconnected) => {
                                return;
                            }
                        }
                    }
                }
            });
        }

        // Producer threads - batch send
        for _ in 0..producer_count {
            let mut tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                let mut sent = 0usize;
                let backoff = crossbeam_utils::Backoff::new();
                while sent < messages_per_producer {
                    let to_send = batch_size.min(messages_per_producer - sent);
                    backoff.reset();
                    loop {
                        match tx.try_send_many(to_send, || std::hint::black_box(0u64)) {
                            Ok(mut batch) => {
                                for i in 0..to_send {
                                    batch.write_next(std::hint::black_box((sent + i) as u64));
                                }
                                batch.finish();
                                sent += to_send;
                                break;
                            }
                            Err(enso_channel::errors::TrySendError::InsufficientCapacity {
                                ..
                            }) => {
                                backoff.spin();
                            }
                            Err(enso_channel::errors::TrySendError::Disconnected) => {
                                panic!("channel disconnected");
                            }
                        }
                    }
                }
            });
        }
    })
    .expect("threads should join");
}

// =============================================================================
// Criterion benchmark functions
// =============================================================================

fn bench_broadcast_p1(c: &mut Criterion) {
    let capacities = bench_config::get_capacities();
    let message_count = bench_config::get_message_count(500_000, 500_000);

    let mut group = c.benchmark_group("broadcast/p1");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));

    for &capacity in &capacities {
        for &n_consumers in BROADCAST_P1_CONSUMER_COUNTS.iter() {
            match n_consumers {
                2 => {
                    register_broadcast_p1_variants::<2>(&mut group, capacity, message_count, &BATCH_SIZES)
                }
                4 => {
                    register_broadcast_p1_variants::<4>(&mut group, capacity, message_count, &BATCH_SIZES)
                }
                _ => unreachable!("unexpected consumer count"),
            }
        }
    }

    group.finish();
}

fn bench_broadcast_pn(c: &mut Criterion) {
    let capacities = bench_config::get_capacities();
    let message_count = bench_config::get_message_count(500_000, 500_000);

    let mut group = c.benchmark_group("broadcast/pN");
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(3));

    for &capacity in &capacities {
        for &(producers, n_consumers) in &BROADCAST_PN_CONFIGS {
            let messages_per_producer = message_count / producers;
            match n_consumers {
                2 => register_broadcast_pn_variants::<2>(
                    &mut group,
                    capacity,
                    producers,
                    messages_per_producer,
                    &BATCH_SIZES,
                ),
                4 => register_broadcast_pn_variants::<4>(
                    &mut group,
                    capacity,
                    producers,
                    messages_per_producer,
                    &BATCH_SIZES,
                ),
                _ => unreachable!("unexpected consumer count"),
            }
        }
    }

    group.finish();
}

criterion_group!(benches, bench_broadcast_p1, bench_broadcast_pn);
criterion_main!(benches);
