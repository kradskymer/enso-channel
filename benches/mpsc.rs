//! Latency benchmark for MPSC channels with Criterion.
//!
//! Metric semantics:
//! - Each Criterion inner iteration executes exactly one burst.
//! - A burst sends and receives exactly `burst_size * producers` messages.
//! - We record a raw per-burst sample (`elapsed_ns`) and report it as burst latency.
//!
//! Env vars:
//! - `ENSO_MPSC_BUFFER_SIZE` (default: 4096, must be power of two)
//! - `ENSO_MPSC_BURST_SIZES` (default: 1,16,64,128)
//! - `ENSO_MPSC_OUTPUT` (default: both) values: csv | table | both
//! - `ENSO_MPSC_TIMEOUT_SECS` (default: 300; 0 disables watchdog)
//! - `ENSO_CHANNEL_PINNING` (default: on)
//! - `ENSO_MPSC_OUTPUT_DIR` (optional, default: benches/results)
//! - `ENSO_MPSC_WARMUP_BURSTS` (default: 10_000)
//! - `ENSO_MPSC_MEASURE_BURSTS` (default: 100_000)
//!
//! Run with:
//! - `cargo bench --bench mpsc`

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, Throughput};
use crossbeam_utils::Backoff;

#[path = "bench_support/mod.rs"]
mod bench_support;

use bench_support::{
    parse_u64_env, parse_usize_env, parse_usize_list_env, resolve_output_dir,
    spawn_timeout_watchdog, write_reports, BurstRecorder, BurstStats, CorePinning, OutputMode,
    ReportRow, DEFAULT_RESULTS_DIR,
};

const DEFAULT_BUFFER_SIZE: usize = 4096;
const DEFAULT_BURST_SIZES: &[usize] = &[1, 16, 64, 128];
const DEFAULT_PRODUCER_GROUPS: &[usize] = &[2, 4];
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_WARMUP_BURSTS: u64 = 10_000;
const DEFAULT_MEASURE_BURSTS: u64 = 100_000;
const BENCH_NAME: &str = "mpsc";

#[derive(Debug)]
struct CrossbeamRunner {
    producers: usize,
    burst_size: usize,
    iter_to_run: Arc<AtomicU64>,
    iter_completed: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    recorder: BurstRecorder,
    producer_handles: Vec<thread::JoinHandle<()>>,
    receiver_handle: Option<thread::JoinHandle<()>>,
}

impl CrossbeamRunner {
    fn new(
        buffer_size: usize,
        producers: usize,
        burst_size: usize,
        pinning: Arc<CorePinning>,
        warmup_bursts: u64,
        measure_bursts: u64,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::bounded::<u64>(buffer_size);
        let iter_to_run = Arc::new(AtomicU64::new(0));
        let iter_completed = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let mut producer_handles = Vec::with_capacity(producers);
        for producer_idx in 0..producers {
            let tx_for_thread = tx.clone();
            let iter_to_run_for_thread = iter_to_run.clone();
            let stop_for_thread = stop.clone();
            let pinning_for_thread = pinning.clone();
            let burst_size_for_thread = burst_size;

            producer_handles.push(thread::spawn(move || {
                // Keep core 0 for main, core 1 for receiver.
                pinning_for_thread.pin_current(producer_idx + 2);
                run_crossbeam_producer(
                    tx_for_thread,
                    iter_to_run_for_thread,
                    stop_for_thread,
                    burst_size_for_thread,
                );
            }));
        }

        let total_messages = burst_size * producers;
        let iter_to_run_for_receiver = iter_to_run.clone();
        let iter_completed_for_receiver = iter_completed.clone();
        let stop_for_receiver = stop.clone();
        let pinning_for_receiver = pinning.clone();

        let receiver_handle = thread::spawn(move || {
            pinning_for_receiver.pin_current(1);
            run_crossbeam_receiver(
                rx,
                iter_to_run_for_receiver,
                iter_completed_for_receiver,
                stop_for_receiver,
                total_messages,
            );
        });

        Self {
            producers,
            burst_size,
            iter_to_run,
            iter_completed,
            stop,
            recorder: BurstRecorder::new(warmup_bursts, measure_bursts),
            producer_handles,
            receiver_handle: Some(receiver_handle),
        }
    }

    fn run_one_burst(&mut self) {
        let backoff = Backoff::new();
        let start = Instant::now();
        let iteration = self.iter_to_run.fetch_add(1, Ordering::AcqRel) + 1;

        backoff.reset();
        loop {
            let completed = self.iter_completed.load(Ordering::Acquire);
            std::hint::black_box(completed);
            if completed >= iteration {
                break;
            }
            backoff.spin();
        }

        let elapsed_ns = start.elapsed().as_nanos() as u64;
        self.recorder.record_burst(elapsed_ns);
    }

    fn stats(&self) -> BurstStats {
        self.recorder.stats()
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
                "warning: recorder incomplete for crossbeam/mpsc (producers={}, burst={}): measured={} target={} remaining={} extra_bursts={} max_extra_bursts={}",
                self.producers,
                self.burst_size,
                self.recorder.measured_samples(),
                self.recorder.target_samples(),
                self.recorder.remaining(),
                extra_bursts,
                max_extra_bursts,
            );
        }
    }
}

impl Drop for CrossbeamRunner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);

        if let Some(handle) = self.receiver_handle.take() {
            let _ = handle.join();
        }

        for handle in self.producer_handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn run_crossbeam_producer(
    tx: crossbeam_channel::Sender<u64>,
    iter_to_run: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    burst_size: usize,
) {
    let backoff = Backoff::new();
    let mut observed_iter = 0u64;

    while !stop.load(Ordering::Acquire) {
        backoff.reset();
        loop {
            let target_iter = iter_to_run.load(Ordering::Acquire);

            if target_iter > observed_iter {
                while observed_iter < target_iter {
                    observed_iter += 1;

                    for _ in 0..burst_size {
                        backoff.reset();
                        loop {
                            if stop.load(Ordering::Acquire) {
                                return;
                            }

                            match tx.try_send(std::hint::black_box(0u64)) {
                                Ok(()) => break,
                                Err(crossbeam_channel::TrySendError::Full(_)) => backoff.spin(),
                                Err(crossbeam_channel::TrySendError::Disconnected(_)) => return,
                            }
                        }
                    }
                }
                break;
            }

            if stop.load(Ordering::Acquire) {
                return;
            }
            backoff.spin();
        }
    }
}

fn run_crossbeam_receiver(
    rx: crossbeam_channel::Receiver<u64>,
    iter_to_run: Arc<AtomicU64>,
    iter_completed: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    total_messages: usize,
) {
    let backoff = Backoff::new();
    let mut observed_iter = 0u64;

    while !stop.load(Ordering::Acquire) {
        backoff.reset();
        loop {
            let target_iter = iter_to_run.load(Ordering::Acquire);

            if target_iter > observed_iter {
                while observed_iter < target_iter {
                    observed_iter += 1;

                    for _ in 0..total_messages {
                        backoff.reset();
                        loop {
                            if stop.load(Ordering::Acquire) {
                                return;
                            }

                            match rx.try_recv() {
                                Ok(value) => {
                                    std::hint::black_box(value);
                                    break;
                                }
                                Err(crossbeam_channel::TryRecvError::Empty) => backoff.spin(),
                                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
                            }
                        }
                    }

                    iter_completed.store(observed_iter, Ordering::Release);
                }
                break;
            }

            if stop.load(Ordering::Acquire) {
                return;
            }
            backoff.spin();
        }
    }
}

#[derive(Debug)]
struct EnsoMpscRunner {
    producers: usize,
    burst_size: usize,
    recv_batch: usize,
    iter_to_run: Arc<AtomicU64>,
    iter_completed: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    recorder: BurstRecorder,
    producer_handles: Vec<thread::JoinHandle<()>>,
    receiver_handle: Option<thread::JoinHandle<()>>,
}

impl EnsoMpscRunner {
    fn new(
        buffer_size: usize,
        producers: usize,
        burst_size: usize,
        recv_batch: usize,
        pinning: Arc<CorePinning>,
        warmup_bursts: u64,
        measure_bursts: u64,
    ) -> Self {
        let (tx, mut rx) = enso_channel::mpsc::channel::<u64>(buffer_size);
        let iter_to_run = Arc::new(AtomicU64::new(0));
        let iter_completed = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let mut producer_handles = Vec::with_capacity(producers);
        for producer_idx in 0..producers {
            let mut tx_for_thread = tx.clone();
            let iter_to_run_for_thread = iter_to_run.clone();
            let stop_for_thread = stop.clone();
            let pinning_for_thread = pinning.clone();
            let burst_size_for_thread = burst_size;

            producer_handles.push(thread::spawn(move || {
                // Keep core 0 for main, core 1 for receiver.
                pinning_for_thread.pin_current(producer_idx + 2);
                run_enso_producer(
                    &mut tx_for_thread,
                    iter_to_run_for_thread,
                    stop_for_thread,
                    burst_size_for_thread,
                );
            }));
        }

        let total_messages = burst_size * producers;
        let recv_batch = recv_batch.clamp(1, total_messages);

        let iter_to_run_for_receiver = iter_to_run.clone();
        let iter_completed_for_receiver = iter_completed.clone();
        let stop_for_receiver = stop.clone();
        let pinning_for_receiver = pinning.clone();

        let receiver_handle = thread::spawn(move || {
            pinning_for_receiver.pin_current(1);
            run_enso_receiver(
                &mut rx,
                iter_to_run_for_receiver,
                iter_completed_for_receiver,
                stop_for_receiver,
                total_messages,
                recv_batch,
            );
        });

        Self {
            producers,
            burst_size,
            recv_batch,
            iter_to_run,
            iter_completed,
            stop,
            recorder: BurstRecorder::new(warmup_bursts, measure_bursts),
            producer_handles,
            receiver_handle: Some(receiver_handle),
        }
    }

    fn run_one_burst(&mut self) {
        let backoff = Backoff::new();
        let start = Instant::now();
        let iteration = self.iter_to_run.fetch_add(1, Ordering::AcqRel) + 1;

        backoff.reset();
        loop {
            let completed = self.iter_completed.load(Ordering::Acquire);
            std::hint::black_box(completed);
            if completed >= iteration {
                break;
            }
            backoff.spin();
        }

        let elapsed_ns = start.elapsed().as_nanos() as u64;
        self.recorder.record_burst(elapsed_ns);
    }

    fn stats(&self) -> BurstStats {
        self.recorder.stats()
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
                "warning: recorder incomplete for enso/mpsc (producers={}, burst={}, recv_batch={}): measured={} target={} remaining={} extra_bursts={} max_extra_bursts={}",
                self.producers,
                self.burst_size,
                self.recv_batch,
                self.recorder.measured_samples(),
                self.recorder.target_samples(),
                self.recorder.remaining(),
                extra_bursts,
                max_extra_bursts,
            );
        }
    }
}

impl Drop for EnsoMpscRunner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);

        if let Some(handle) = self.receiver_handle.take() {
            let _ = handle.join();
        }

        for handle in self.producer_handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn run_enso_producer(
    tx: &mut enso_channel::mpsc::Sender<u64>,
    iter_to_run: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    burst_size: usize,
) {
    let backoff = Backoff::new();
    let mut observed_iter = 0u64;

    while !stop.load(Ordering::Acquire) {
        backoff.reset();
        loop {
            let target_iter = iter_to_run.load(Ordering::Acquire);

            if target_iter > observed_iter {
                while observed_iter < target_iter {
                    observed_iter += 1;

                    let mut sent = 0usize;
                    while sent < burst_size {
                        backoff.reset();
                        loop {
                            if stop.load(Ordering::Acquire) {
                                return;
                            }

                            let remaining = burst_size - sent;
                            match tx.try_send_at_most(remaining, || std::hint::black_box(0u64)) {
                                Ok(batch) => {
                                    sent += batch.capacity();
                                    break;
                                }
                                Err(enso_channel::errors::TrySendAtMostError::Full) => {
                                    backoff.spin()
                                }
                                Err(enso_channel::errors::TrySendAtMostError::Disconnected) => {
                                    return;
                                }
                            }
                        }
                    }
                }
                break;
            }

            if stop.load(Ordering::Acquire) {
                return;
            }
            backoff.spin();
        }
    }
}

fn run_enso_receiver(
    rx: &mut enso_channel::mpsc::Receiver<u64>,
    iter_to_run: Arc<AtomicU64>,
    iter_completed: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    total_messages: usize,
    recv_batch: usize,
) {
    let backoff = Backoff::new();
    let mut observed_iter = 0u64;

    while !stop.load(Ordering::Acquire) {
        backoff.reset();
        loop {
            let target_iter = iter_to_run.load(Ordering::Acquire);

            if target_iter > observed_iter {
                while observed_iter < target_iter {
                    observed_iter += 1;

                    let mut received = 0usize;
                    while received < total_messages {
                        backoff.reset();
                        loop {
                            if stop.load(Ordering::Acquire) {
                                return;
                            }

                            let remaining = total_messages - received;
                            let limit = remaining.min(recv_batch);
                            match rx.try_recv_at_most(limit) {
                                Ok(iter) => {
                                    let mut chunk = 0usize;
                                    for guard in iter {
                                        std::hint::black_box(*guard);
                                        chunk += 1;
                                    }
                                    received += chunk;
                                    break;
                                }
                                Err(enso_channel::errors::TryRecvAtMostError::Empty) => {
                                    backoff.spin()
                                }
                                Err(enso_channel::errors::TryRecvAtMostError::Disconnected) => {
                                    return;
                                }
                            }
                        }
                    }

                    iter_completed.store(observed_iter, Ordering::Release);
                }
                break;
            }

            if stop.load(Ordering::Acquire) {
                return;
            }
            backoff.spin();
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
        let buffer_size = parse_usize_env("ENSO_MPSC_BUFFER_SIZE", DEFAULT_BUFFER_SIZE);
        let burst_sizes = parse_usize_list_env("ENSO_MPSC_BURST_SIZES", DEFAULT_BURST_SIZES);
        let output_mode = OutputMode::parse_env("ENSO_MPSC_OUTPUT", "both");
        let timeout_secs = parse_u64_env("ENSO_MPSC_TIMEOUT_SECS", DEFAULT_TIMEOUT_SECS);
        let output_dir = resolve_output_dir("ENSO_MPSC_OUTPUT_DIR", DEFAULT_RESULTS_DIR);
        let warmup_bursts = parse_u64_env("ENSO_MPSC_WARMUP_BURSTS", DEFAULT_WARMUP_BURSTS);
        let measure_bursts = parse_u64_env("ENSO_MPSC_MEASURE_BURSTS", DEFAULT_MEASURE_BURSTS);

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

fn bench_latency_mpsc(
    c: &mut Criterion,
    settings: &BenchSettings,
    pinning: Arc<CorePinning>,
) -> Vec<ReportRow> {
    let mut rows = Vec::<ReportRow>::new();
    let mut group = c.benchmark_group(BENCH_NAME);

    for &producers in DEFAULT_PRODUCER_GROUPS {
        for &burst_size in &settings.burst_sizes {
            group.throughput(Throughput::Elements(1));

            let mut scenario = CrossbeamRunner::new(
                settings.buffer_size,
                producers,
                burst_size,
                pinning.clone(),
                settings.warmup_bursts,
                settings.measure_bursts,
            );

            group.bench_function(
                BenchmarkId::new(format!("crossbeam/p{}", producers), burst_size),
                |b| {
                    b.iter(|| {
                        scenario.run_one_burst();
                    })
                },
            );

            scenario.fill_recorder();

            rows.push(ReportRow {
                scenario: "crossbeam-channel".to_string(),
                producers,
                consumers: 1,
                buffer_size: settings.buffer_size,
                burst_size,
                stats: scenario.stats(),
            });
        }
    }

    for &producers in DEFAULT_PRODUCER_GROUPS {
        for &burst_size in &settings.burst_sizes {
            group.throughput(Throughput::Elements(1));

            // Consumer receives are benchmarked with batching enabled.
            // Keep `recv_batch` independent of producer count for clearer interpretation.
            // let recv_batch = (burst_size / 2).max(1);
            let recv_batch = burst_size;

            let mut scenario = EnsoMpscRunner::new(
                settings.buffer_size,
                producers,
                burst_size,
                recv_batch,
                pinning.clone(),
                settings.warmup_bursts,
                settings.measure_bursts,
            );

            group.bench_function(
                BenchmarkId::new(format!("enso_channel/p{}", producers), burst_size),
                |b| {
                    b.iter(|| {
                        scenario.run_one_burst();
                    })
                },
            );

            scenario.fill_recorder();

            rows.push(ReportRow {
                scenario: format!(
                    "enso_channel send_batch={} recv_batch={} (at most)",
                    burst_size, recv_batch
                ),
                producers,
                consumers: 1,
                buffer_size: settings.buffer_size,
                burst_size,
                stats: scenario.stats(),
            });
        }
    }

    group.finish();
    rows
}

fn main() {
    let settings = BenchSettings::from_env();

    if !settings.buffer_size.is_power_of_two() {
        eprintln!(
            "ENSO_MPSC_BUFFER_SIZE must be a power of two (got {}).",
            settings.buffer_size
        );
        std::process::exit(2);
    }

    spawn_timeout_watchdog(settings.timeout_secs, BENCH_NAME);

    let pinning = Arc::new(CorePinning::from_env("ENSO_CHANNEL_PINNING"));
    pinning.pin_current(0);

    let mut criterion = Criterion::default().configure_from_args();
    let rows = bench_latency_mpsc(&mut criterion, &settings, pinning);

    criterion.final_summary();
    write_reports(
        &rows,
        settings.output_mode,
        &settings.output_dir,
        BENCH_NAME,
    );
}
