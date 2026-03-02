//! Latency benchmark for SPSC (SPSC, MPSC) channels with Criterion.
//!
//! Metric semantics:
//! - Each Criterion inner iteration executes exactly one burst.
//! - One burst means send+receive exactly `burst_size` messages.
//! - We record a raw per-burst sample (`elapsed_ns`) and report it as burst latency.
//!
//! Env vars:
//! - `ENSO_SPSC_BUFFER_SIZE` (default: 4096, must be power of two)
//! - `ENSO_SPSC_BURST_SIZES` (default: 1,16,64,128)
//! - `ENSO_SPSC_OUTPUT` (default: both) values: csv | table | both
//! - `ENSO_SPSC_TIMEOUT_SECS` (default: 300; 0 disables watchdog)
//! - `ENSO_CHANNEL_PINNING` (default: on)
//! - `ENSO_SPSC_OUTPUT_DIR` (optional, default: benches/results)
//! - `ENSO_SPSC_WARMUP_BURSTS` (default: 10_000)
//! - `ENSO_SPSC_MEASURE_BURSTS` (default: 100_000)
//!
//! Run with:
//! - `cargo bench --bench spsc`

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Instant;

use criterion::{BenchmarkId, Criterion, Throughput};
use crossbeam_utils::Backoff;

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
const BENCH_NAME: &str = "spsc";

#[derive(Debug)]
struct CrossbeamRunner {
    burst_size: usize,
    tx: Option<crossbeam_channel::Sender<u64>>,
    sink: Arc<AtomicU64>,
    next_value: u64,
    stop: Arc<AtomicBool>,
    recorder: BurstRecorder,
    receiver: Option<thread::JoinHandle<()>>,
}

impl CrossbeamRunner {
    fn new(
        buffer_size: usize,
        burst_size: usize,
        pinning: Arc<CorePinning>,
        warmup_bursts: u64,
        measure_bursts: u64,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::bounded::<u64>(buffer_size);
        let sink = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let sink_for_receiver = sink.clone();
        let stop_for_receiver = stop.clone();

        let receiver = thread::spawn(move || {
            pinning.pin_current(1);
            run_crossbeam_receiver(rx, sink_for_receiver, stop_for_receiver);
        });

        Self {
            burst_size,
            tx: Some(tx),
            sink,
            next_value: 1,
            stop,
            recorder: BurstRecorder::new(warmup_bursts, measure_bursts),
            receiver: Some(receiver),
        }
    }

    fn run_one_burst(&mut self) {
        let backoff = Backoff::new();
        let start = Instant::now();
        let first = self.next_value;
        let last = first + self.burst_size as u64 - 1;

        let tx = self
            .tx
            .as_ref()
            .expect("crossbeam sender missing during benchmark iteration");

        for value in first..=last {
            backoff.reset();
            loop {
                match tx.try_send(std::hint::black_box(value)) {
                    Ok(()) => break,
                    Err(crossbeam_channel::TrySendError::Full(_)) => backoff.spin(),
                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                        panic!("crossbeam receiver disconnected")
                    }
                }
            }
        }

        self.next_value = last + 1;

        backoff.reset();
        loop {
            let observed = self.sink.load(Ordering::Acquire);
            std::hint::black_box(observed);
            if observed >= last {
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
                "warning: recorder incomplete for crossbeam/spsc (burst={}): measured={} target={} remaining={} extra_bursts={} max_extra_bursts={}",
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
        let _ = self.tx.take();
        if let Some(handle) = self.receiver.take() {
            let _ = handle.join();
        }
    }
}

fn run_crossbeam_receiver(
    rx: crossbeam_channel::Receiver<u64>,
    sink: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) {
    let backoff = Backoff::new();

    while !stop.load(Ordering::Acquire) {
        backoff.reset();
        loop {
            match rx.try_recv() {
                Ok(value) => {
                    sink.store(value, Ordering::Release);
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    backoff.spin();
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    return;
                }
            }
        }
    }
}

struct EnsoMpscRunner {
    burst_size: usize,
    batch_size: usize,
    recv_chunk: usize,
    tx: Option<enso_channel::mpsc::Sender<u64>>,
    sink: Arc<AtomicU64>,
    next_value: u64,
    stop: Arc<AtomicBool>,
    recorder: BurstRecorder,
    receiver: Option<thread::JoinHandle<()>>,
}

impl EnsoMpscRunner {
    fn new(
        buffer_size: usize,
        burst_size: usize,
        batch_size: usize,
        recv_chunk: usize,
        pinning: Arc<CorePinning>,
        warmup_bursts: u64,
        measure_bursts: u64,
    ) -> Self {
        let (tx, mut rx) = enso_channel::mpsc::channel::<u64>(buffer_size);
        let sink = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let recv_chunk = recv_chunk.clamp(1, burst_size);

        let sink_for_receiver = sink.clone();
        let stop_for_receiver = stop.clone();

        let receiver = thread::spawn(move || {
            pinning.pin_current(1);
            run_enso_receiver(
                &mut rx,
                sink_for_receiver,
                stop_for_receiver,
                batch_size,
                recv_chunk,
            );
        });

        Self {
            burst_size,
            batch_size,
            recv_chunk,
            tx: Some(tx),
            sink,
            next_value: 1,
            stop,
            recorder: BurstRecorder::new(warmup_bursts, measure_bursts),
            receiver: Some(receiver),
        }
    }

    fn run_one_burst(&mut self) {
        let backoff = Backoff::new();
        let start = Instant::now();
        let tx = self
            .tx
            .as_mut()
            .expect("enso sender missing during benchmark iteration");

        let mut next = self.next_value;
        let last = next + self.burst_size as u64 - 1;

        if self.batch_size == 1 {
            for _ in 0..self.burst_size {
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(next)) {
                        Ok(()) => {
                            next += 1;
                            break;
                        }
                        Err(enso_channel::errors::TrySendError::InsufficientCapacity {
                            ..
                        }) => backoff.spin(),
                        Err(enso_channel::errors::TrySendError::Disconnected) => {
                            panic!("enso spsc receiver disconnected")
                        }
                    }
                }
            }
        } else {
            let mut sent = 0usize;
            while sent < self.burst_size {
                let remaining = self.burst_size - sent;
                let to_send = remaining.min(self.batch_size);
                backoff.reset();

                loop {
                    match tx.try_send_many(to_send, || std::hint::black_box(0u64)) {
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
                        Err(enso_channel::errors::TrySendError::InsufficientCapacity {
                            ..
                        }) => backoff.spin(),
                        Err(enso_channel::errors::TrySendError::Disconnected) => {
                            panic!("enso spsc receiver disconnected")
                        }
                    }
                }
            }
        }

        self.next_value = last + 1;

        backoff.reset();
        loop {
            let observed = self.sink.load(Ordering::Acquire);
            std::hint::black_box(observed);
            if observed >= last {
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
                "warning: recorder incomplete for enso/spsc (burst={}, batch={}, recv_chunk={}): measured={} target={} remaining={} extra_bursts={} max_extra_bursts={}",
                self.burst_size,
                self.batch_size,
                self.recv_chunk,
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
        let _ = self.tx.take();
        if let Some(handle) = self.receiver.take() {
            let _ = handle.join();
        }
    }
}

fn run_enso_receiver(
    rx: &mut enso_channel::mpsc::Receiver<u64>,
    sink: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    batch_size: usize,
    recv_chunk: usize,
) {
    let backoff = Backoff::new();

    while !stop.load(Ordering::Acquire) {
        backoff.reset();

        if batch_size == 1 {
            loop {
                match rx.try_recv() {
                    Ok(value) => {
                        sink.store(*value, Ordering::Release);
                        break;
                    }
                    Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        backoff.spin();
                    }
                    Err(enso_channel::errors::TryRecvError::Disconnected) => {
                        return;
                    }
                }
            }
        } else {
            loop {
                match rx.try_recv_many(recv_chunk) {
                    Ok(iter) => {
                        for guard in iter {
                            sink.store(*guard, Ordering::Release);
                        }
                        break;
                    }
                    Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        backoff.spin();
                    }
                    Err(enso_channel::errors::TryRecvError::Disconnected) => {
                        return;
                    }
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
        let buffer_size = parse_usize_env("ENSO_SPSC_BUFFER_SIZE", DEFAULT_BUFFER_SIZE);
        let burst_sizes = parse_usize_list_env("ENSO_SPSC_BURST_SIZES", DEFAULT_BURST_SIZES);
        let output_mode = OutputMode::parse_env("ENSO_SPSC_OUTPUT", "both");
        let timeout_secs = parse_u64_env("ENSO_SPSC_TIMEOUT_SECS", DEFAULT_TIMEOUT_SECS);
        let output_dir = resolve_output_dir("ENSO_SPSC_OUTPUT_DIR", DEFAULT_RESULTS_DIR);
        let warmup_bursts = parse_u64_env("ENSO_SPSC_WARMUP_BURSTS", DEFAULT_WARMUP_BURSTS);
        let measure_bursts = parse_u64_env("ENSO_SPSC_MEASURE_BURSTS", DEFAULT_MEASURE_BURSTS);

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

fn bench_latency_spsc(
    c: &mut Criterion,
    settings: &BenchSettings,
    pinning: Arc<CorePinning>,
) -> Vec<ReportRow> {
    let mut rows = Vec::<ReportRow>::new();
    let mut group = c.benchmark_group(BENCH_NAME);

    for &burst_size in &settings.burst_sizes {
        group.throughput(Throughput::Elements(1));

        let mut scenario = CrossbeamRunner::new(
            settings.buffer_size,
            burst_size,
            pinning.clone(),
            settings.warmup_bursts,
            settings.measure_bursts,
        );

        group.bench_function(BenchmarkId::new("crossbeam", burst_size), |b| {
            b.iter(|| {
                scenario.run_one_burst();
            })
        });

        scenario.fill_recorder();

        rows.push(ReportRow {
            scenario: "crossbeam-channel".to_string(),
            producers: 1,
            consumers: 1,
            buffer_size: settings.buffer_size,
            burst_size,
            stats: scenario.stats(),
        });
    }

    for &burst_size in &settings.burst_sizes {
        group.throughput(Throughput::Elements(1));

        let batch_size = (burst_size / 2).max(1);
        let recv_chunk = batch_size;
        let mut scenario = EnsoMpscRunner::new(
            settings.buffer_size,
            burst_size,
            batch_size,
            recv_chunk,
            pinning.clone(),
            settings.warmup_bursts,
            settings.measure_bursts,
        );

        group.bench_function(BenchmarkId::new("enso-spsc", burst_size), |b| {
            b.iter(|| {
                scenario.run_one_burst();
            })
        });

        scenario.fill_recorder();

        rows.push(ReportRow {
            scenario: format!(
                "enso_channel send_batch={} recv_batch={}",
                batch_size, recv_chunk
            ),
            producers: 1,
            consumers: 1,
            buffer_size: settings.buffer_size,
            burst_size,
            stats: scenario.stats(),
        });
    }

    group.finish();
    rows
}

fn main() {
    let settings = BenchSettings::from_env();

    if !settings.buffer_size.is_power_of_two() {
        eprintln!(
            "ENSO_SPSC_BUFFER_SIZE must be a power of two (got {}).",
            settings.buffer_size
        );
        std::process::exit(2);
    }

    spawn_timeout_watchdog(settings.timeout_secs, BENCH_NAME);

    let pinning = Arc::new(CorePinning::from_env("ENSO_CHANNEL_PINNING"));
    pinning.pin_current(0);

    let mut criterion = Criterion::default().configure_from_args();
    let rows = bench_latency_spsc(&mut criterion, &settings, pinning);

    criterion.final_summary();
    write_reports(
        &rows,
        settings.output_mode,
        &settings.output_dir,
        BENCH_NAME,
    );
}
