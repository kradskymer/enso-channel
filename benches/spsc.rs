//! SPSC channel benchmarks comparing enso_channel against crossbeam-channel and flume.

use std::sync::Barrier;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use crossbeam_utils::Backoff;

pub mod bench_config;

// =============================================================================
// Benchmark configuration
// =============================================================================

// Configuration constants (edit these to change bench parameters)
const CAPACITIES: &[usize] = &[1024, 4096];
const BATCH_SIZES: &[usize] = &[4, 16, 64];
// Mode defaults. Default is SHORT mode; set `ENSO_CHANNEL_BENCH_LONG=1` to select LONG mode.
const SHORT_MESSAGE_COUNT: usize = 50_000;
const SHORT_WARMUP_MS: u64 = 1000; // ms
const SHORT_MEASURE_SECS: u64 = 1; // s

const LONG_MESSAGE_COUNT: usize = 1_000_000;
const LONG_WARMUP_MS: u64 = 5000; // ms
const LONG_MEASURE_SECS: u64 = 10; // s

// =============================================================================
// Channel abstractions for benchmarking
// =============================================================================

trait BenchChannel: Sized {
    type Sender: Send;
    type Receiver: Send;

    fn name() -> &'static str;
    fn bounded(capacity: usize) -> (Self::Sender, Self::Receiver);
    fn send(tx: &mut Self::Sender, value: u64);
    fn recv(rx: &mut Self::Receiver) -> u64;
}

// -----------------------------------------------------------------------------
// enso_channel::mpsc
// -----------------------------------------------------------------------------

struct RChannelsSpsc;

impl BenchChannel for RChannelsSpsc {
    type Sender = enso_channel::mpsc::Sender<u64>;
    type Receiver = enso_channel::mpsc::Receiver<u64>;

    fn name() -> &'static str {
        "enso_channel"
    }

    fn bounded(capacity: usize) -> (Self::Sender, Self::Receiver) {
        enso_channel::mpsc::channel(capacity)
    }

    fn send(tx: &mut Self::Sender, value: u64) {
        let backoff = Backoff::new();
        loop {
            match tx.try_send(std::hint::black_box(value)) {
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

    fn recv(rx: &mut Self::Receiver) -> u64 {
        let backoff = Backoff::new();
        loop {
            match rx.try_recv() {
                Ok(guard) => return *guard,
                Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                    backoff.spin();
                }
                Err(enso_channel::errors::TryRecvError::Disconnected) => {
                    panic!("channel disconnected");
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// crossbeam-channel
// -----------------------------------------------------------------------------

struct CrossbeamSpsc;

impl BenchChannel for CrossbeamSpsc {
    type Sender = crossbeam_channel::Sender<u64>;
    type Receiver = crossbeam_channel::Receiver<u64>;

    fn name() -> &'static str {
        "crossbeam"
    }

    fn bounded(capacity: usize) -> (Self::Sender, Self::Receiver) {
        crossbeam_channel::bounded(capacity)
    }

    fn send(tx: &mut Self::Sender, value: u64) {
        let backoff = Backoff::new();
        loop {
            match tx.try_send(std::hint::black_box(value)) {
                Ok(()) => return,
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    backoff.spin();
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    panic!("channel disconnected");
                }
            }
        }
    }

    fn recv(rx: &mut Self::Receiver) -> u64 {
        let backoff = Backoff::new();
        loop {
            match rx.try_recv() {
                Ok(v) => return v,
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    backoff.spin();
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    panic!("channel disconnected");
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// flume
// -----------------------------------------------------------------------------

struct FlumeSpsc;

impl BenchChannel for FlumeSpsc {
    type Sender = flume::Sender<u64>;
    type Receiver = flume::Receiver<u64>;

    fn name() -> &'static str {
        "flume"
    }

    fn bounded(capacity: usize) -> (Self::Sender, Self::Receiver) {
        flume::bounded(capacity)
    }

    fn send(tx: &mut Self::Sender, value: u64) {
        let backoff = Backoff::new();
        loop {
            match tx.try_send(std::hint::black_box(value)) {
                Ok(()) => return,
                Err(flume::TrySendError::Full(_)) => {
                    backoff.spin();
                }
                Err(flume::TrySendError::Disconnected(_)) => {
                    panic!("channel disconnected");
                }
            }
        }
    }

    fn recv(rx: &mut Self::Receiver) -> u64 {
        let backoff = Backoff::new();
        loop {
            match rx.try_recv() {
                Ok(v) => return v,
                Err(flume::TryRecvError::Empty) => {
                    backoff.spin();
                }
                Err(flume::TryRecvError::Disconnected) => {
                    panic!("channel disconnected");
                }
            }
        }
    }
}

// =============================================================================
// Benchmark scenarios
// =============================================================================

/// Concurrent producer/consumer: both run in parallel threads.
fn run_concurrent<C: BenchChannel>(capacity: usize, message_count: usize) {
    let (mut tx, mut rx) = C::bounded(capacity);
    let barrier = Barrier::new(2);
    let barrier = &barrier;

    crossbeam_utils::thread::scope(|s| {
        // Producer thread
        s.spawn(|_| {
            barrier.wait();
            for i in 0..message_count as u64 {
                C::send(&mut tx, i);
            }
        });

        // Consumer thread
        s.spawn(|_| {
            barrier.wait();
            for _ in 0..message_count {
                std::hint::black_box(C::recv(&mut rx));
            }
        });
    })
    .expect("threads should join");
}

/// Burst: producer sends all, then consumer receives all.
/// Tests throughput without contention.
fn run_burst<C: BenchChannel>(capacity: usize, burst_size: usize) {
    let (mut tx, mut rx) = C::bounded(capacity);

    // Send burst (up to capacity)
    let to_send = burst_size.min(capacity);
    for i in 0..to_send as u64 {
        C::send(&mut tx, i);
    }

    // Receive burst
    for _ in 0..to_send {
        std::hint::black_box(C::recv(&mut rx));
    }
}

// =============================================================================
// Batch benchmark scenarios (enso_channel only - others don't have batch API)
// =============================================================================

/// Batch burst: producer sends all in batches, then consumer receives all in batches.
fn run_batch_burst(capacity: usize, batch_size: usize) {
    let (mut tx, mut rx) = enso_channel::mpsc::channel::<u64>(capacity);

    // Send in batches
    let mut sent = 0usize;
    while sent < capacity {
        let to_send = batch_size.min(capacity - sent);
        let mut batch = tx
            .try_send_many(to_send, || std::hint::black_box(0u64))
            .expect("should have space");
        for i in 0..to_send {
            batch.write_next(std::hint::black_box((sent + i) as u64));
        }
        batch.finish();
        sent += to_send;
    }

    // Receive in batches
    let mut received = 0usize;
    while received < capacity {
        let to_recv = batch_size.min(capacity - received);
        let iter = rx.try_recv_many(to_recv).expect("should have items");
        for v in iter {
            std::hint::black_box(*v);
        }
        received += to_recv;
    }
}

// =============================================================================
// Criterion benchmark functions
// =============================================================================

fn bench_spsc_concurrent(c: &mut Criterion) {
    // let capacities = get_capacities();
    let message_count = bench_config::get_message_count(SHORT_MESSAGE_COUNT, LONG_MESSAGE_COUNT);

    let mut group = c.benchmark_group("spsc/concurrent");
    group.warm_up_time(bench_config::get_warmup_time(
        SHORT_WARMUP_MS,
        LONG_WARMUP_MS,
    ));
    group.measurement_time(bench_config::get_measurement_time(
        SHORT_MEASURE_SECS,
        LONG_MEASURE_SECS,
    ));
    group.throughput(Throughput::Elements(message_count as u64));

    for &capacity in CAPACITIES {
        // enso_channel
        group.bench_with_input(
            BenchmarkId::new(RChannelsSpsc::name(), capacity),
            &capacity,
            |b, &cap| {
                b.iter(|| run_concurrent::<RChannelsSpsc>(cap, message_count));
            },
        );

        // crossbeam
        group.bench_with_input(
            BenchmarkId::new(CrossbeamSpsc::name(), capacity),
            &capacity,
            |b, &cap| {
                b.iter(|| run_concurrent::<CrossbeamSpsc>(cap, message_count));
            },
        );

        // flume
        group.bench_with_input(
            BenchmarkId::new(FlumeSpsc::name(), capacity),
            &capacity,
            |b, &cap| {
                b.iter(|| run_concurrent::<FlumeSpsc>(cap, message_count));
            },
        );
    }

    group.finish();
}

fn bench_spsc_burst(c: &mut Criterion) {
    // let capacities = get_capacities();

    let mut group = c.benchmark_group("spsc/burst");
    group.warm_up_time(bench_config::get_warmup_time(
        SHORT_WARMUP_MS,
        LONG_WARMUP_MS,
    ));
    group.measurement_time(bench_config::get_measurement_time(
        SHORT_MEASURE_SECS,
        LONG_MEASURE_SECS,
    ));

    for &capacity in CAPACITIES {
        let burst_size = capacity; // Fill to capacity
        group.throughput(Throughput::Elements(burst_size as u64));

        // enso_channel single-item burst
        group.bench_with_input(
            BenchmarkId::new(RChannelsSpsc::name(), capacity),
            &capacity,
            |b, &cap| {
                b.iter(|| run_burst::<RChannelsSpsc>(cap, burst_size));
            },
        );

        // enso_channel batch bursts (different batch sizes)
        for &batch_size in BATCH_SIZES {
            let param = format!("cap={}/batch={}", capacity, batch_size);
            group.bench_with_input(
                BenchmarkId::new("enso_channel_batch", &param),
                &capacity,
                move |b, &cap| {
                    b.iter(|| run_batch_burst(cap, batch_size));
                },
            );
        }

        // crossbeam single-item burst for reference
        group.bench_with_input(
            BenchmarkId::new(CrossbeamSpsc::name(), capacity),
            &capacity,
            |b, &cap| {
                b.iter(|| run_burst::<CrossbeamSpsc>(cap, burst_size));
            },
        );

        // flume single-item burst for reference
        group.bench_with_input(
            BenchmarkId::new(FlumeSpsc::name(), capacity),
            &capacity,
            |b, &cap| {
                b.iter(|| run_burst::<FlumeSpsc>(cap, burst_size));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_spsc_concurrent, bench_spsc_burst);
criterion_main!(benches);
