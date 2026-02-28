//! MPMC benchmarks comparing enso_channel against crossbeam-channel and flume.
//!
//! Tests the work-distribution pattern where each item is processed by exactly one consumer.

use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

mod bench_config;

// =============================================================================
// Benchmark configuration
// =============================================================================

// Bench defaults extracted as constants for easy reuse and clarity
const DEFAULT_MESSAGE_COUNT: usize = 500_000;
const WARMUP_MS: u64 = 500;
const MEASURE_SECS: u64 = 3;
const BATCH_SIZES: &[usize] = &[16, 64];
const WORK_DISTRIBUTION_CONFIGS: &[(usize, usize)] = &[
    (1, 2), // single producer
    (1, 4),
    (2, 2), // Balanced
    (2, 4),
    (4, 2),
    (4, 4),
];
const BATCH_BENCH_CONFIGS: &[(usize, usize)] = &[(2, 2), (4, 4)];

// parse_usize/parse_usize_list/get_capacities/get_message_count moved to `bench_config` helpers.
// Use `bench_config::parse_usize`, `bench_config::parse_usize_list`, `bench_config::get_capacities`,
// and `bench_config::get_message_count(short_default, long_default)` instead.

// =============================================================================
// Benchmark scenarios - enso_channel
// =============================================================================

fn run_enso_channel_mpmc(
    capacity: usize,
    producer_count: usize,
    consumer_count: usize,
    messages_per_producer: usize,
) {
    let (tx, rx) = enso_channel::mpmc::channel::<u64>(capacity);
    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(producer_count + consumer_count);
    let barrier = &barrier;
    let send_counter = AtomicU64::new(0);
    let send_counter = &send_counter;
    let recv_counter = AtomicUsize::new(0);
    let recv_counter = &recv_counter;

    crossbeam_utils::thread::scope(|s| {
        // Producer threads
        for _ in 0..producer_count {
            let mut tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                for _ in 0..messages_per_producer {
                    let seq = send_counter.fetch_add(1, Ordering::Relaxed);
                    let backoff = crossbeam_utils::Backoff::new();
                    loop {
                        match tx.try_send(std::hint::black_box(seq)) {
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

        // Consumer threads (compete for items)
        for _ in 0..consumer_count {
            let mut rx = rx.clone();
            s.spawn(move |_| {
                barrier.wait();
                let backoff = crossbeam_utils::Backoff::new();
                loop {
                    match rx.try_recv() {
                        Ok(guard) => {
                            backoff.reset();
                            std::hint::black_box(*guard);
                            let count = recv_counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if count >= total_messages {
                                return;
                            }
                        }
                        Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                            if recv_counter.load(Ordering::Relaxed) >= total_messages {
                                return;
                            }
                            backoff.spin();
                        }
                        Err(enso_channel::errors::TryRecvError::Disconnected) => {
                            return;
                        }
                    }
                }
            });
        }
    })
    .expect("threads should join");
}

// =============================================================================
// Benchmark scenarios - crossbeam-channel
// =============================================================================

fn run_crossbeam_mpmc(
    capacity: usize,
    producer_count: usize,
    consumer_count: usize,
    messages_per_producer: usize,
) {
    let (tx, rx) = crossbeam_channel::bounded::<u64>(capacity);
    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(producer_count + consumer_count);
    let barrier = &barrier;
    let send_counter = AtomicU64::new(0);
    let send_counter = &send_counter;
    let recv_counter = AtomicUsize::new(0);
    let recv_counter = &recv_counter;

    crossbeam_utils::thread::scope(|s| {
        // Producer threads
        for _ in 0..producer_count {
            let tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                for _ in 0..messages_per_producer {
                    let backoff = crossbeam_utils::Backoff::new();
                    let seq = send_counter.fetch_add(1, Ordering::Relaxed);
                    loop {
                        match tx.try_send(std::hint::black_box(seq)) {
                            Ok(()) => break,
                            Err(crossbeam_channel::TrySendError::Full(_)) => {
                                backoff.spin();
                            }
                            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                                panic!("channel disconnected");
                            }
                        }
                    }
                }
            });
        }

        // Consumer threads (compete for items)
        for _ in 0..consumer_count {
            let rx = rx.clone();
            s.spawn(move |_| {
                barrier.wait();
                let backoff = crossbeam_utils::Backoff::new();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            backoff.reset();
                            std::hint::black_box(v);
                            let count = recv_counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if count >= total_messages {
                                return;
                            }
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => {
                            if recv_counter.load(Ordering::Relaxed) >= total_messages {
                                return;
                            }
                            backoff.spin();
                        }
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            return;
                        }
                    }
                }
            });
        }
    })
    .expect("threads should join");
}

// =============================================================================
// Benchmark scenarios - flume
// =============================================================================

fn run_flume_mpmc(
    capacity: usize,
    producer_count: usize,
    consumer_count: usize,
    messages_per_producer: usize,
) {
    let (tx, rx) = flume::bounded::<u64>(capacity);
    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(producer_count + consumer_count);
    let barrier = &barrier;
    let send_counter = AtomicU64::new(0);
    let send_counter = &send_counter;
    let recv_counter = AtomicUsize::new(0);
    let recv_counter = &recv_counter;

    crossbeam_utils::thread::scope(|s| {
        // Producer threads
        for _ in 0..producer_count {
            let tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                for _ in 0..messages_per_producer {
                    let seq = send_counter.fetch_add(1, Ordering::Relaxed);
                    let backoff = crossbeam_utils::Backoff::new();
                    loop {
                        match tx.try_send(std::hint::black_box(seq)) {
                            Ok(()) => break,
                            Err(flume::TrySendError::Full(_)) => {
                                backoff.spin();
                            }
                            Err(flume::TrySendError::Disconnected(_)) => {
                                panic!("channel disconnected");
                            }
                        }
                    }
                }
            });
        }

        // Consumer threads (compete for items)
        for _ in 0..consumer_count {
            let rx = rx.clone();
            s.spawn(move |_| {
                barrier.wait();
                let backoff = crossbeam_utils::Backoff::new();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            backoff.reset();
                            std::hint::black_box(v);
                            let count = recv_counter.fetch_add(1, Ordering::Relaxed) + 1;
                            if count >= total_messages {
                                return;
                            }
                        }
                        Err(flume::TryRecvError::Empty) => {
                            if recv_counter.load(Ordering::Relaxed) >= total_messages {
                                return;
                            }
                            backoff.spin();
                        }
                        Err(flume::TryRecvError::Disconnected) => {
                            return;
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

fn run_enso_channel_mpmc_batch(
    capacity: usize,
    producer_count: usize,
    consumer_count: usize,
    messages_per_producer: usize,
    batch_size: usize,
) {
    let (tx, rx) = enso_channel::mpmc::channel::<u64>(capacity);
    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(producer_count + consumer_count);
    let barrier = &barrier;
    let send_counter = AtomicU64::new(0);
    let send_counter = &send_counter;
    let recv_counter = AtomicUsize::new(0);
    let recv_counter = &recv_counter;

    crossbeam_utils::thread::scope(|s| {
        // Producer threads - batch send
        for _ in 0..producer_count {
            let mut tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                let mut sent = 0;
                while sent < messages_per_producer {
                    let to_send = batch_size.min(messages_per_producer - sent);
                    let backoff = crossbeam_utils::Backoff::new();
                    loop {
                        match tx.try_send_many(to_send, || std::hint::black_box(0u64)) {
                            Ok(mut batch) => {
                                for _ in 0..to_send {
                                    let seq = send_counter.fetch_add(1, Ordering::Relaxed);
                                    batch.write_next(std::hint::black_box(seq));
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

        // Consumer threads - batch receive
        for _ in 0..consumer_count {
            let mut rx = rx.clone();
            s.spawn(move |_| {
                barrier.wait();
                let backoff = crossbeam_utils::Backoff::new();
                loop {
                    let remaining =
                        total_messages.saturating_sub(recv_counter.load(Ordering::Relaxed));
                    if remaining == 0 {
                        return;
                    }
                    let to_recv = batch_size.min(remaining);
                    match rx.try_recv_many(to_recv) {
                        Ok(iter) => {
                            backoff.reset();
                            let mut count = 0;
                            for v in iter {
                                std::hint::black_box(*v);
                                count += 1;
                            }
                            let total_count =
                                recv_counter.fetch_add(count, Ordering::Relaxed) + count;
                            if total_count >= total_messages {
                                return;
                            }
                        }
                        Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                            if recv_counter.load(Ordering::Relaxed) >= total_messages {
                                return;
                            }
                            backoff.spin();
                        }
                        Err(enso_channel::errors::TryRecvError::Disconnected) => {
                            return;
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

fn bench_mpmc_work_distribution(c: &mut Criterion) {
    let capacities = bench_config::get_capacities();
    let message_count =
        bench_config::get_message_count(DEFAULT_MESSAGE_COUNT, DEFAULT_MESSAGE_COUNT);

    // Test various producer/consumer configurations
    let configs = WORK_DISTRIBUTION_CONFIGS;

    let mut group = c.benchmark_group("mpmc/work_distribution");
    group.warm_up_time(Duration::from_millis(WARMUP_MS));
    group.measurement_time(Duration::from_secs(MEASURE_SECS));

    for &capacity in &capacities {
        for &(producers, consumers) in configs {
            let messages_per_producer = message_count / producers;
            let total_messages = producers * messages_per_producer;
            group.throughput(Throughput::Elements(total_messages as u64));

            let scenario = format!("cap_{capacity}/p{producers}_c{consumers}");

            // enso_channel
            group.bench_with_input(
                BenchmarkId::new("enso_channel", &scenario),
                &(capacity, producers, consumers, messages_per_producer),
                |b, &(cap, prod, cons, mpp)| {
                    b.iter(|| run_enso_channel_mpmc(cap, prod, cons, mpp));
                },
            );

            // crossbeam
            group.bench_with_input(
                BenchmarkId::new("crossbeam", &scenario),
                &(capacity, producers, consumers, messages_per_producer),
                |b, &(cap, prod, cons, mpp)| {
                    b.iter(|| run_crossbeam_mpmc(cap, prod, cons, mpp));
                },
            );

            // flume
            group.bench_with_input(
                BenchmarkId::new("flume", &scenario),
                &(capacity, producers, consumers, messages_per_producer),
                |b, &(cap, prod, cons, mpp)| {
                    b.iter(|| run_flume_mpmc(cap, prod, cons, mpp));
                },
            );
        }
    }

    group.finish();
}

/// Batch MPMC benchmark - enso_channel only
fn bench_mpmc_batch_vs_single(c: &mut Criterion) {
    let capacities = bench_config::get_capacities();
    let message_count =
        bench_config::get_message_count(DEFAULT_MESSAGE_COUNT, DEFAULT_MESSAGE_COUNT);
    let batch_sizes = BATCH_SIZES;

    // Test a subset of configs for batch comparison
    let configs = BATCH_BENCH_CONFIGS;

    let mut group = c.benchmark_group("mpmc/batch_vs_single");
    group.warm_up_time(Duration::from_millis(WARMUP_MS));
    group.measurement_time(Duration::from_secs(MEASURE_SECS));

    for &capacity in &capacities {
        for &(producers, consumers) in configs {
            let messages_per_producer = message_count / producers;
            let total_messages = producers * messages_per_producer;
            group.throughput(Throughput::Elements(total_messages as u64));

            let scenario_base = format!("cap_{capacity}/p{producers}_c{consumers}");

            // enso_channel single-item baseline
            group.bench_with_input(
                BenchmarkId::new("enso_channel_single", &scenario_base),
                &(capacity, producers, consumers, messages_per_producer),
                |b, &(cap, prod, cons, mpp)| {
                    b.iter(|| run_enso_channel_mpmc(cap, prod, cons, mpp));
                },
            );

            // enso_channel batch variants
            for &batch_size in batch_sizes {
                let scenario_batch = format!("{scenario_base}/b{batch_size}");
                group.bench_with_input(
                    BenchmarkId::new("enso_channel_batch", &scenario_batch),
                    &(
                        capacity,
                        producers,
                        consumers,
                        messages_per_producer,
                        batch_size,
                    ),
                    |b, &(cap, prod, cons, mpp, bs)| {
                        b.iter(|| run_enso_channel_mpmc_batch(cap, prod, cons, mpp, bs));
                    },
                );
            }

            // crossbeam for reference
            group.bench_with_input(
                BenchmarkId::new("crossbeam", &scenario_base),
                &(capacity, producers, consumers, messages_per_producer),
                |b, &(cap, prod, cons, mpp)| {
                    b.iter(|| run_crossbeam_mpmc(cap, prod, cons, mpp));
                },
            );
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_mpmc_work_distribution,
    bench_mpmc_batch_vs_single
);
criterion_main!(benches);
