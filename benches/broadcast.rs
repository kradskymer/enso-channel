//! Latency benchmark for enso_channel broadcast (SPMC scalability) with Criterion.
//!
//! Metric semantics:
//! - Each Criterion inner iteration executes exactly one burst.
//! - A burst sends exactly `burst_size` messages from one producer.
//! - Each consumer receives the full burst (broadcast semantics).
//! - We record a raw per-burst sample (`elapsed_ns`) and report it as burst latency (ns/burst).
//!
//! Env vars:
//! - `ENSO_BROADCAST_BUFFER_SIZE` (default: 4096, must be power of two)
//! - `ENSO_BROADCAST_BURST_SIZES` (default: 1,16,64,128)
//! - `ENSO_BROADCAST_OUTPUT` (default: both) values: csv | table | both
//! - `ENSO_BROADCAST_TIMEOUT_SECS` (default: 300; 0 disables watchdog)
//! - `ENSO_CHANNEL_PINNING` (default: on)
//! - `ENSO_BROADCAST_OUTPUT_DIR` (optional, default: benches/results)
//! - `ENSO_BROADCAST_WARMUP_BURSTS` (default: 10_000)
//! - `ENSO_BROADCAST_MEASURE_BURSTS` (default: 100_000)
//!
//! Run with:
//! - `cargo bench --bench broadcast`

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, Throughput};
use crossbeam_utils::{Backoff, CachePadded};

#[path = "bench_support/mod.rs"]
mod bench_support;

use bench_support::{
    BurstRecorder, BurstStats, CorePinning, DEFAULT_RESULTS_DIR, OutputMode, ReportRow,
    parse_u64_env, parse_usize_env, parse_usize_list_env, resolve_output_dir,
    spawn_timeout_watchdog, write_reports,
};

const DEFAULT_BUFFER_SIZE: usize = 4096;
const DEFAULT_BURST_SIZES: &[usize] = &[1, 16, 64, 128];
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_WARMUP_BURSTS: u64 = 10_000;
const DEFAULT_MEASURE_BURSTS: u64 = 100_000;
const BENCH_NAME: &str = "broadcast";

struct EnsoBroadcastRunner<const N: usize> {
    burst_size: usize,
    send_limit: usize,
    recv_limit: usize,
    sender: Option<enso_channel::broadcast::Sender<u64, N>>,
    last_seen: [Arc<CachePadded<AtomicU64>>; N],
    next_value: u64,
    stop: Arc<AtomicBool>,
    recorder: BurstRecorder,
    consumer_handles: Vec<thread::JoinHandle<()>>,
}

impl<const N: usize> EnsoBroadcastRunner<N> {
    fn new(
        buffer_size: usize,
        burst_size: usize,
        send_limit: usize,
        recv_limit: usize,
        pinning: Arc<CorePinning>,
        warmup_bursts: u64,
        measure_bursts: u64,
    ) -> Self {
        let (sender, receivers): (
            enso_channel::broadcast::Sender<u64, N>,
            [enso_channel::broadcast::Receiver<u64>; N],
        ) = enso_channel::broadcast::channel(buffer_size);

        let stop = Arc::new(AtomicBool::new(false));
        let last_seen: [Arc<CachePadded<AtomicU64>>; N] =
            std::array::from_fn(|_| Arc::new(CachePadded::new(AtomicU64::new(0))));

        let mut consumer_handles = Vec::with_capacity(N);
        for (consumer_idx, (mut rx, sink)) in
            receivers.into_iter().zip(last_seen.iter()).enumerate()
        {
            let stop_for_thread = stop.clone();
            let sink_for_thread = sink.clone();
            let pinning_for_thread = pinning.clone();
            consumer_handles.push(thread::spawn(move || {
                pinning_for_thread.pin_current(consumer_idx + 1);
                run_consumer_loop(&mut rx, recv_limit, sink_for_thread, stop_for_thread);
            }));
        }

        Self {
            burst_size,
            send_limit: send_limit.max(1),
            recv_limit: recv_limit.max(1),
            sender: Some(sender),
            last_seen,
            next_value: 1,
            stop,
            recorder: BurstRecorder::new(warmup_bursts, measure_bursts),
            consumer_handles,
        }
    }

    fn run_one_burst(&mut self) {
        let backoff = Backoff::new();
        let start = Instant::now();

        let first = self.next_value;
        let last = first + self.burst_size as u64 - 1;

        let sender = match self.sender.as_mut() {
            Some(sender) => sender,
            None => panic!("broadcast sender missing during benchmark iteration"),
        };

        let mut sent = 0usize;
        let mut next = first;
        while sent < self.burst_size {
            let remaining = self.burst_size - sent;
            let limit = remaining.min(self.send_limit);
            backoff.reset();

            loop {
                match sender.try_send_at_most(limit, || std::hint::black_box(0u64)) {
                    Ok(mut batch) => {
                        let writes = batch.capacity();
                        for _ in 0..writes {
                            batch.write_next(std::hint::black_box(next));
                            next += 1;
                        }
                        batch.finish();
                        sent += writes;
                        break;
                    }
                    Err(enso_channel::errors::TrySendAtMostError::Full) => backoff.spin(),
                    Err(enso_channel::errors::TrySendAtMostError::Disconnected) => {
                        panic!("broadcast receiver disconnected")
                    }
                }
            }
        }

        self.next_value = last + 1;

        backoff.reset();
        loop {
            let mut all_caught_up = true;
            for sink in &self.last_seen {
                let observed = sink.load(Ordering::Acquire);
                std::hint::black_box(observed);
                if observed < last {
                    all_caught_up = false;
                    break;
                }
            }

            if all_caught_up {
                break;
            }
            backoff.spin();
        }

        let elapsed_ns = start.elapsed().as_nanos() as u64;
        self.recorder.record_burst(elapsed_ns);
    }

    fn fill_recorder(&mut self) {
        if self.recorder.is_complete() {
            return;
        }

        let max_extra_bursts = self.recorder.warmup_bursts() + self.recorder.target_samples();
        let mut extra_bursts = 0u64;

        while !self.recorder.is_complete() && extra_bursts < max_extra_bursts {
            self.run_one_burst();
            extra_bursts += 1;
        }

        if !self.recorder.is_complete() {
            eprintln!(
                "warning: recorder incomplete for enso/broadcast (consumers={}, burst={}, send_limit={}, recv_limit={}): measured={} target={} remaining={} extra_bursts={} max_extra_bursts={}",
                N,
                self.burst_size,
                self.send_limit,
                self.recv_limit,
                self.recorder.measured_samples(),
                self.recorder.target_samples(),
                self.recorder.remaining(),
                extra_bursts,
                max_extra_bursts,
            );
        }
    }

    fn stats(&self) -> BurstStats {
        self.recorder.stats()
    }
}

impl<const N: usize> Drop for EnsoBroadcastRunner<N> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);

        let _ = self.sender.take();

        for handle in self.consumer_handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn run_consumer_loop(
    rx: &mut enso_channel::broadcast::Receiver<u64>,
    recv_limit: usize,
    sink: Arc<CachePadded<AtomicU64>>,
    stop: Arc<AtomicBool>,
) {
    let backoff = Backoff::new();

    while !stop.load(Ordering::Acquire) {
        backoff.reset();
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }

            match rx.try_recv_at_most(recv_limit) {
                Ok(iter) => {
                    let mut latest = None;
                    for value in iter {
                        latest = Some(*black_box(value));
                    }

                    if let Some(last) = latest {
                        sink.store(last, Ordering::Release);
                    }
                    break;
                }
                Err(enso_channel::errors::TryRecvAtMostError::Empty) => backoff.spin(),
                Err(enso_channel::errors::TryRecvAtMostError::Disconnected) => {
                    return;
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
struct BenchSettings {
    buffer_size: usize,
    burst_sizes: Vec<usize>,
    output_mode: OutputMode,
    timeout_secs: u64,
    output_dir: std::path::PathBuf,
    warmup_bursts: u64,
    measure_bursts: u64,
}

impl BenchSettings {
    fn from_env() -> Self {
        let buffer_size = parse_usize_env("ENSO_BROADCAST_BUFFER_SIZE", DEFAULT_BUFFER_SIZE);
        let burst_sizes = parse_usize_list_env("ENSO_BROADCAST_BURST_SIZES", DEFAULT_BURST_SIZES);
        let output_mode = OutputMode::parse_env("ENSO_BROADCAST_OUTPUT", "both");
        let timeout_secs = parse_u64_env("ENSO_BROADCAST_TIMEOUT_SECS", DEFAULT_TIMEOUT_SECS);
        let output_dir = resolve_output_dir("ENSO_BROADCAST_OUTPUT_DIR", DEFAULT_RESULTS_DIR);
        let warmup_bursts = parse_u64_env("ENSO_BROADCAST_WARMUP_BURSTS", DEFAULT_WARMUP_BURSTS);
        let measure_bursts = parse_u64_env("ENSO_BROADCAST_MEASURE_BURSTS", DEFAULT_MEASURE_BURSTS);

        Self {
            buffer_size,
            burst_sizes,
            output_mode,
            timeout_secs,
            output_dir,
            warmup_bursts,
            measure_bursts,
        }
    }
}

fn bench_topology<const N: usize>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    settings: &BenchSettings,
    pinning: Arc<CorePinning>,
    rows: &mut Vec<ReportRow>,
) {
    for &burst_size in &settings.burst_sizes {
        let send_limit = (burst_size / 2).max(1);
        let recv_limit = (burst_size / 2).max(1);

        group.throughput(Throughput::Elements(1));

        let mut scenario = EnsoBroadcastRunner::<N>::new(
            settings.buffer_size,
            burst_size,
            send_limit,
            recv_limit,
            pinning.clone(),
            settings.warmup_bursts,
            settings.measure_bursts,
        );

        group.bench_function(
            BenchmarkId::new(
                format!(
                    "enso_channel::broadcast/c{}_sl{}_rl{}",
                    N, send_limit, recv_limit
                ),
                burst_size,
            ),
            |b| {
                b.iter(|| {
                    scenario.run_one_burst();
                })
            },
        );

        scenario.fill_recorder();

        rows.push(ReportRow {
            scenario: format!(
                "enso_channel::broadcast send_limit={} recv_limit={}",
                send_limit, recv_limit
            ),
            producers: 1,
            consumers: N,
            buffer_size: settings.buffer_size,
            burst_size,
            stats: scenario.stats(),
        });
    }
}

fn bench_latency_broadcast(
    c: &mut Criterion,
    settings: &BenchSettings,
    pinning: Arc<CorePinning>,
) -> Vec<ReportRow> {
    let mut rows = Vec::<ReportRow>::new();
    let mut group = c.benchmark_group(BENCH_NAME);

    bench_topology::<2>(&mut group, settings, pinning.clone(), &mut rows);
    bench_topology::<4>(&mut group, settings, pinning, &mut rows);

    group.finish();
    rows
}

fn main() {
    let settings = BenchSettings::from_env();

    if !settings.buffer_size.is_power_of_two() {
        eprintln!(
            "ENSO_BROADCAST_BUFFER_SIZE must be a power of two (got {}).",
            settings.buffer_size
        );
        std::process::exit(2);
    }

    spawn_timeout_watchdog(settings.timeout_secs, BENCH_NAME);

    let pinning = Arc::new(CorePinning::from_env("ENSO_CHANNEL_PINNING"));
    pinning.pin_current(0);

    let mut criterion = Criterion::default().configure_from_args();
    let rows = bench_latency_broadcast(&mut criterion, &settings, pinning);

    criterion.final_summary();
    write_reports(
        &rows,
        settings.output_mode,
        &settings.output_dir,
        BENCH_NAME,
    );
}
