//! MPSC channel benchmarks comparing enso_channel against crossbeam-channel and flume.

use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use crossbeam_utils::Backoff;

pub mod bench_config;

// =============================================================================
// Benchmark configuration
// =============================================================================

// Mode defaults. Default is SHORT mode; set `ENSO_CHANNEL_BENCH_LONG=1` to select LONG mode.
const SHORT_MESSAGE_COUNT: usize = 50_000;
const SHORT_WARMUP_MS: u64 = 1000; // ms
const SHORT_MEASURE_SECS: u64 = 1; // s
const SHORT_SAMPLES: usize = 20;

const LONG_MESSAGE_COUNT: usize = 500_000;
const LONG_WARMUP_MS: u64 = 500; // ms
const LONG_MEASURE_SECS: u64 = 3; // s
const LONG_SAMPLES: usize = 100;

// =============================================================================
// Benchmark scenarios - enso_channel
// =============================================================================

fn run_enso_channel_mpsc(capacity: usize, producer_count: usize, messages_per_producer: usize) {
    let (tx, mut rx) = enso_channel::mpsc::channel::<u64>(capacity);
    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(producer_count + 1);
    let barrier = &barrier;
    let counter = AtomicU64::new(0);
    let counter = &counter;

    crossbeam_utils::thread::scope(|s| {
        // Producer threads
        for _ in 0..producer_count {
            let mut tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                for _ in 0..messages_per_producer {
                    let seq = counter.fetch_add(1, Ordering::Relaxed);
                    let backoff = Backoff::new();
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

        // Consumer thread
        s.spawn(|_| {
            barrier.wait();
            for _ in 0..total_messages {
                let backoff = Backoff::new();
                loop {
                    match rx.try_recv() {
                        Ok(guard) => {
                            std::hint::black_box(*guard);
                            break;
                        }
                        Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                            backoff.spin();
                        }
                        Err(enso_channel::errors::TryRecvError::Disconnected) => {
                            panic!("channel disconnected");
                        }
                    }
                }
            }
        });
    })
    .expect("threads should join");
}

// =============================================================================
// Benchmark scenarios - crossbeam-channel
// =============================================================================

fn run_crossbeam_mpsc(capacity: usize, producer_count: usize, messages_per_producer: usize) {
    let (tx, rx) = crossbeam_channel::bounded::<u64>(capacity);
    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(producer_count + 1);
    let barrier = &barrier;
    let counter = AtomicU64::new(0);
    let counter = &counter;

    crossbeam_utils::thread::scope(|s| {
        // Producer threads
        for _ in 0..producer_count {
            let tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                for _ in 0..messages_per_producer {
                    let seq = counter.fetch_add(1, Ordering::Relaxed);
                    let backoff = Backoff::new();
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

        // Consumer thread
        s.spawn(|_| {
            barrier.wait();
            for _ in 0..total_messages {
                let backoff = Backoff::new();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(v);
                            break;
                        }
                        Err(crossbeam_channel::TryRecvError::Empty) => {
                            backoff.spin();
                        }
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            panic!("channel disconnected");
                        }
                    }
                }
            }
        });
    })
    .expect("threads should join");
}

// =============================================================================
// Benchmark scenarios - flume
// =============================================================================

fn run_flume_mpsc(capacity: usize, producer_count: usize, messages_per_producer: usize) {
    let (tx, rx) = flume::bounded::<u64>(capacity);
    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(producer_count + 1);
    let barrier = &barrier;
    let counter = AtomicU64::new(0);
    let counter = &counter;

    crossbeam_utils::thread::scope(|s| {
        // Producer threads
        for _ in 0..producer_count {
            let tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                for _ in 0..messages_per_producer {
                    let seq = counter.fetch_add(1, Ordering::Relaxed);
                    let backoff = Backoff::new();
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

        // Consumer thread
        s.spawn(|_| {
            barrier.wait();
            for _ in 0..total_messages {
                let backoff = Backoff::new();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(v);
                            break;
                        }
                        Err(flume::TryRecvError::Empty) => {
                            backoff.spin();
                        }
                        Err(flume::TryRecvError::Disconnected) => {
                            panic!("channel disconnected");
                        }
                    }
                }
            }
        });
    })
    .expect("threads should join");
}

// =============================================================================
// Batch benchmark scenarios - enso_channel only (unique batch API advantage)
// =============================================================================

fn run_enso_channel_mpsc_batch(
    capacity: usize,
    producer_count: usize,
    messages_per_producer: usize,
    batch_size: usize,
) {
    let (tx, mut rx) = enso_channel::mpsc::channel::<u64>(capacity);
    let total_messages = producer_count * messages_per_producer;
    let barrier = Barrier::new(producer_count + 1);
    let barrier = &barrier;
    let counter = AtomicU64::new(0);
    let counter = &counter;

    crossbeam_utils::thread::scope(|s| {
        // Producer threads - batch send
        for _ in 0..producer_count {
            let mut tx = tx.clone();
            s.spawn(move |_| {
                barrier.wait();
                let mut sent = 0;
                while sent < messages_per_producer {
                    let to_send = batch_size.min(messages_per_producer - sent);
                    let backoff = Backoff::new();
                    loop {
                        match tx.try_send_many(to_send, || std::hint::black_box(0u64)) {
                            Ok(mut batch) => {
                                for _ in 0..to_send {
                                    let seq = counter.fetch_add(1, Ordering::Relaxed);
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

        // Consumer thread - batch receive
        s.spawn(|_| {
            barrier.wait();
            let mut received = 0;
            while received < total_messages {
                let to_recv = batch_size.min(total_messages - received);
                let backoff = Backoff::new();
                loop {
                    match rx.try_recv_many(to_recv) {
                        Ok(iter) => {
                            for v in iter {
                                std::hint::black_box(*v);
                            }
                            received += to_recv;
                            break;
                        }
                        Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                            backoff.spin();
                        }
                        Err(enso_channel::errors::TryRecvError::Disconnected) => {
                            panic!("channel disconnected");
                        }
                    }
                }
            }
        });
    })
    .expect("threads should join");
}

// =============================================================================
// Criterion benchmark functions
// =============================================================================

fn bench_mpsc_multi_producer(c: &mut Criterion) {
    let capacities = bench_config::get_capacities();
    let message_count = bench_config::get_message_count(SHORT_MESSAGE_COUNT, LONG_MESSAGE_COUNT);
    let producer_counts = bench_config::get_producer_counts();

    let mut group = c.benchmark_group("mpsc/multi_producer");
    group.sample_size(bench_config::get_sample_size(SHORT_SAMPLES, LONG_SAMPLES));
    group.warm_up_time(bench_config::get_warmup_time(
        SHORT_WARMUP_MS,
        LONG_WARMUP_MS,
    ));
    group.measurement_time(bench_config::get_measurement_time(
        SHORT_MEASURE_SECS,
        LONG_MEASURE_SECS,
    ));

    for &capacity in &capacities {
        for &producers in &producer_counts {
            let messages_per_producer = message_count / producers;
            let total_messages = producers * messages_per_producer;
            group.throughput(Throughput::Elements(total_messages as u64));

            let scenario = format!("cap_{capacity}/p{producers}");

            // enso_channel
            group.bench_with_input(
                BenchmarkId::new("enso_channel", &scenario),
                &(capacity, producers, messages_per_producer),
                |b, &(cap, prod, mpp)| {
                    b.iter(|| run_enso_channel_mpsc(cap, prod, mpp));
                },
            );

            // crossbeam
            group.bench_with_input(
                BenchmarkId::new("crossbeam", &scenario),
                &(capacity, producers, messages_per_producer),
                |b, &(cap, prod, mpp)| {
                    b.iter(|| run_crossbeam_mpsc(cap, prod, mpp));
                },
            );

            // flume
            group.bench_with_input(
                BenchmarkId::new("flume", &scenario),
                &(capacity, producers, messages_per_producer),
                |b, &(cap, prod, mpp)| {
                    b.iter(|| run_flume_mpsc(cap, prod, mpp));
                },
            );
        }
    }

    group.finish();
}

/// Batch MPSC benchmark - enso_channel only, comparing batch vs single-item
fn bench_mpsc_batch_vs_single(c: &mut Criterion) {
    let capacities = bench_config::get_capacities();
    let message_count = bench_config::get_message_count(SHORT_MESSAGE_COUNT, LONG_MESSAGE_COUNT);
    let producer_counts = bench_config::get_producer_counts();
    let batch_sizes = [16, 64];

    let mut group = c.benchmark_group("mpsc/batch_vs_single");
    group.sample_size(bench_config::get_sample_size(SHORT_SAMPLES, LONG_SAMPLES));
    group.warm_up_time(bench_config::get_warmup_time(
        SHORT_WARMUP_MS,
        LONG_WARMUP_MS,
    ));
    group.measurement_time(bench_config::get_measurement_time(
        SHORT_MEASURE_SECS,
        LONG_MEASURE_SECS,
    ));

    for &capacity in &capacities {
        for &producers in &producer_counts {
            let messages_per_producer = message_count / producers;
            let total_messages = producers * messages_per_producer;
            group.throughput(Throughput::Elements(total_messages as u64));

            let scenario_base = format!("cap_{capacity}/p{producers}");

            // enso_channel single-item baseline
            group.bench_with_input(
                BenchmarkId::new("enso_channel_single", &scenario_base),
                &(capacity, producers, messages_per_producer),
                |b, &(cap, prod, mpp)| {
                    b.iter(|| run_enso_channel_mpsc(cap, prod, mpp));
                },
            );

            // enso_channel batch variants
            for &batch_size in &batch_sizes {
                let scenario_batch = format!("cap_{capacity}/p{producers}/b{batch_size}");
                group.bench_with_input(
                    BenchmarkId::new("enso_channel_batch", &scenario_batch),
                    &(capacity, producers, messages_per_producer, batch_size),
                    |b, &(cap, prod, mpp, bs)| {
                        b.iter(|| run_enso_channel_mpsc_batch(cap, prod, mpp, bs));
                    },
                );
            }

            // crossbeam for reference
            group.bench_with_input(
                BenchmarkId::new("crossbeam", &scenario_base),
                &(capacity, producers, messages_per_producer),
                |b, &(cap, prod, mpp)| {
                    b.iter(|| run_crossbeam_mpsc(cap, prod, mpp));
                },
            );
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_mpsc_multi_producer,
    bench_mpsc_batch_vs_single
);
criterion_main!(benches);
