use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use core_affinity::CoreId;
use hdrhistogram::Histogram;

pub const DEFAULT_RESULTS_DIR: &str = "benches/results";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Csv,
    Table,
    Both,
}

impl OutputMode {
    pub fn parse_env(env_key: &str, default: &str) -> Self {
        let value = std::env::var(env_key).unwrap_or_else(|_| default.to_string());
        match value.as_str() {
            "csv" => Self::Csv,
            "table" => Self::Table,
            "both" => Self::Both,
            _ => Self::Both,
        }
    }

    pub fn wants_csv(self) -> bool {
        matches!(self, Self::Csv | Self::Both)
    }

    pub fn wants_table(self) -> bool {
        matches!(self, Self::Table | Self::Both)
    }
}

pub fn parse_usize_env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

pub fn parse_u64_env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

#[allow(dead_code)]
pub fn parse_usize_list_env(name: &str, default: &[usize]) -> Vec<usize> {
    let Some(raw) = std::env::var(name).ok() else {
        return default.to_vec();
    };

    let mut values = Vec::new();
    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }

        match trimmed.parse::<usize>() {
            Ok(parsed) if parsed > 0 => values.push(parsed),
            _ => {}
        }
    }

    if values.is_empty() {
        default.to_vec()
    } else {
        values
    }
}

pub fn pinning_enabled(env_key: &str) -> bool {
    match std::env::var(env_key) {
        Ok(v) => !matches!(v.as_str(), "0" | "off" | "false" | "no"),
        Err(_) => true,
    }
}

pub struct CorePinning {
    enabled: bool,
    cores: &'static [CoreId],
}

impl CorePinning {
    pub fn from_env(env_key: &str) -> Self {
        static AVAILABLE_CORES: OnceLock<Vec<CoreId>> = OnceLock::new();
        let enabled = pinning_enabled(env_key);
        let cores = AVAILABLE_CORES
            .get_or_init(|| core_affinity::get_core_ids().unwrap_or_default())
            .as_slice();

        Self { enabled, cores }
    }

    pub fn pin_current(&self, thread_index: usize) {
        if !self.enabled || self.cores.is_empty() {
            return;
        }

        let core = self.cores[thread_index % self.cores.len()];
        let _ = core_affinity::set_for_current(core);
    }
}

pub fn spawn_timeout_watchdog(timeout_secs: u64, bench_name: &'static str) {
    if timeout_secs == 0 {
        return;
    }

    thread::spawn(move || {
        thread::sleep(Duration::from_secs(timeout_secs));
        eprintln!(
            "{}: timeout after {}s — aborting process",
            bench_name, timeout_secs
        );
        std::process::abort();
    });
}

pub fn resolve_output_dir(env_key: &str, default_path: &str) -> PathBuf {
    std::env::var(env_key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(default_path))
}

pub fn ensure_output_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

#[derive(Debug, Clone)]
pub struct BurstStats {
    pub samples: u64,
    pub mean_ns: f64,
    pub p50_ns: u64,
    pub p99_ns: u64,
    pub p99_9_ns: u64,
    pub max_ns: u64,
}

impl BurstStats {
    pub fn empty() -> Self {
        Self {
            samples: 0,
            mean_ns: 0.0,
            p50_ns: 0,
            p99_ns: 0,
            p99_9_ns: 0,
            max_ns: 0,
        }
    }
}

fn new_histogram() -> Histogram<u64> {
    Histogram::<u64>::new(3).expect("failed to create histogram")
}

fn record_sample(hist: &mut Histogram<u64>, value: u64) {
    if let Err(_e) = hist.record(value) {
        hist.saturating_record(value);
    }
}

pub fn burst_stats_from_hist(hist: &Histogram<u64>) -> BurstStats {
    if hist.is_empty() {
        return BurstStats::empty();
    }

    BurstStats {
        samples: hist.len(),
        mean_ns: hist.mean(),
        p50_ns: hist.value_at_quantile(0.50),
        p99_ns: hist.value_at_quantile(0.99),
        p99_9_ns: hist.value_at_quantile(0.999),
        max_ns: hist.max(),
    }
}

#[derive(Debug)]
pub struct BurstRecorder {
    bursts_seen: u64,
    warmup_bursts: u64,
    measure_bursts: u64,
    hist: Histogram<u64>,
}

impl BurstRecorder {
    pub fn new(warmup_bursts: u64, measure_bursts: u64) -> Self {
        Self {
            bursts_seen: 0,
            warmup_bursts,
            measure_bursts,
            hist: new_histogram(),
        }
    }

    /// Record the elapsed latency for a single burst, measured in nanoseconds.
    pub fn record_burst(&mut self, burst_elapsed_ns: u64) {
        self.bursts_seen += 1;

        if self.bursts_seen > self.warmup_bursts && self.hist.len() < self.measure_bursts {
            record_sample(&mut self.hist, burst_elapsed_ns);
        }
    }

    pub fn stats(&self) -> BurstStats {
        burst_stats_from_hist(&self.hist)
    }

    pub fn is_complete(&self) -> bool {
        self.hist.len() >= self.measure_bursts
    }

    pub fn measured_samples(&self) -> u64 {
        self.hist.len()
    }

    pub fn warmup_bursts(&self) -> u64 {
        self.warmup_bursts
    }

    pub fn target_samples(&self) -> u64 {
        self.measure_bursts
    }

    pub fn remaining(&self) -> u64 {
        self.measure_bursts.saturating_sub(self.hist.len())
    }
}

#[derive(Debug, Clone)]
pub struct ReportRow {
    pub scenario: String,
    pub producers: usize,
    pub consumers: usize,
    pub buffer_size: usize,
    pub burst_size: usize,
    pub stats: BurstStats,
}

fn csv_escape(field: &str) -> String {
    if !field.contains([',', '"', '\n', '\r']) {
        return field.to_string();
    }

    let mut escaped = String::with_capacity(field.len() + 2);
    escaped.push('"');
    for ch in field.chars() {
        match ch {
            '"' => escaped.push_str("\"\""),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

pub fn render_csv(rows: &[ReportRow]) -> String {
    let mut out = String::new();
    out.push_str("Scenario,P,C,buffer,burst,samples,mean,p50,p99,p99.9,max\n");

    for row in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{:.3},{},{},{},{}\n",
            csv_escape(&row.scenario),
            row.producers,
            row.consumers,
            row.buffer_size,
            row.burst_size,
            row.stats.samples,
            row.stats.mean_ns,
            row.stats.p50_ns,
            row.stats.p99_ns,
            row.stats.p99_9_ns,
            row.stats.max_ns,
        ));
    }

    out
}

pub fn render_table(rows: &[ReportRow]) -> String {
    let scenario_width = rows
        .iter()
        .map(|row| row.scenario.len())
        .max()
        .unwrap_or("Scenario".len())
        .clamp(12, 64);

    let mut out = String::new();
    out.push_str("#\n");
    out.push_str("# Table: latency per burst (ns)\n");
    out.push_str(&format!(
        "# {scenario:<w$} {p:>3} {c:>3} {buffer:>6} {burst:>6} {samples:>7} {mean:>7} {p50:>7} {p99:>7} {p999:>7} {max:>7}\n",
        scenario = "Scenario",
        w = scenario_width,
        p = "P",
        c = "C",
        buffer = "buffer",
        burst = "burst",
        samples = "samples",
        mean = "mean",
        p50 = "p50",
        p99 = "p99",
        p999 = "p99.9",
        max = "max",
    ));

    for row in rows {
        out.push_str(&format!(
            "# {scenario:<w$} {p:>3} {c:>3} {buffer:>6} {burst:>6} {samples:>7} {mean:>7.1} {p50:>7} {p99:>7} {p999:>7} {max:>7}\n",
            scenario = row.scenario,
            w = scenario_width,
            p = row.producers,
            c = row.consumers,
            buffer = row.buffer_size,
            burst = row.burst_size,
            samples = row.stats.samples,
            mean = row.stats.mean_ns,
            p50 = row.stats.p50_ns,
            p99 = row.stats.p99_ns,
            p999 = row.stats.p99_9_ns,
            max = row.stats.max_ns,
        ));
    }

    out
}

pub fn write_reports(
    rows: &[ReportRow],
    output_mode: OutputMode,
    output_dir: &Path,
    file_stem: &str,
) {
    if let Err(error) = ensure_output_dir(output_dir) {
        eprintln!(
            "failed to create output directory {:?}: {}",
            output_dir, error
        );
        return;
    }

    if output_mode.wants_csv() {
        let csv_path = output_dir.join(format!("{}.csv", file_stem));
        let csv = render_csv(rows);
        if let Err(error) = std::fs::write(&csv_path, &csv) {
            eprintln!("failed to write CSV report {:?}: {}", csv_path, error);
        }
        if let Err(error) = write!(std::io::stdout(), "{}", csv) {
            eprintln!("failed to write CSV report to stdout: {}", error);
        }
    }

    if output_mode.wants_table() {
        let table_path = output_dir.join(format!("{}.txt", file_stem));
        let table = render_table(rows);
        if let Err(error) = std::fs::write(&table_path, &table) {
            eprintln!("failed to write table report {:?}: {}", table_path, error);
        }
        if let Err(error) = write!(std::io::stdout(), "{}", table) {
            eprintln!("failed to write table report to stdout: {}", error);
        }
    }
}
