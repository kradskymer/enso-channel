//! Tail latency benchmarks for enso_channel vs competitors.
//!
//! This benchmark measures Round-Trip Time (RTT) latency using a ping-pong pattern,
//! reporting percentiles (p50, p99, p99.9) which are critical for latency-sensitive systems.
//!
//! Why not criterion? Criterion focuses on mean/median with confidence intervals,
//! but doesn't directly provide tail percentiles. For latency analysis, we need
//! raw sample collection to compute p99/p99.9 accurately.
//!
//! Run with: `cargo bench --bench latency`

use std::sync::Barrier;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Instant;

use core_affinity::CoreId;
use crossbeam_utils::Backoff;

// =============================================================================
// Configuration
// =============================================================================

const WARMUP_ITERS: usize = 50_000;
const MEASURE_ITERS: usize = 500_000;
const BUFFER_SIZE: usize = 1024 * 16;

// =============================================================================
// Latency Statistics
// =============================================================================

#[derive(Debug, Clone)]
struct LatencyStats {
    min: u64,
    p50: u64,
    p99: u64,
    p99_9: u64,
    max: u64,
    mean: f64,
}

impl LatencyStats {
    fn from_samples(mut samples: Vec<u64>) -> Self {
        samples.sort_unstable();
        let len = samples.len();
        let sum: u64 = samples.iter().sum();

        Self {
            min: samples[0],
            p50: samples[len * 50 / 100],
            p99: samples[len * 99 / 100],
            p99_9: samples[len * 999 / 1000],
            max: samples[len - 1],
            mean: sum as f64 / len as f64,
        }
    }

    fn amortized(&self, batch_size: usize) -> Self {
        Self {
            min: self.min / batch_size as u64,
            p50: self.p50 / batch_size as u64,
            p99: self.p99 / batch_size as u64,
            p99_9: self.p99_9 / batch_size as u64,
            max: self.max / batch_size as u64,
            mean: self.mean / batch_size as f64,
        }
    }
}

// =============================================================================
// CPU Pinning Helpers
// =============================================================================

fn get_core_ids() -> (CoreId, CoreId) {
    let cores = core_affinity::get_core_ids().unwrap_or_default();
    if cores.len() >= 2 {
        // Use two different physical cores for producer/consumer
        (cores[0], cores[1])
    } else if !cores.is_empty() {
        (cores[0], cores[0])
    } else {
        // Fallback: no pinning available
        (CoreId { id: 0 }, CoreId { id: 0 })
    }
}

fn pin_to_core(core: CoreId) {
    let _ = core_affinity::set_for_current(core);
}

// =============================================================================
// Output Formatting
// =============================================================================

const SCENARIO_COL_WIDTH: usize = 48;
const TABLE_WIDTH: usize = SCENARIO_COL_WIDTH + 69; // scenario + separators + six 8-char columns

fn print_section(title: &str) {
    println!();
    println!("=== {} ===", title);
    println!(
        "{:<width$} | {:>8} | {:>8} | {:>8} | {:>8} | {:>8} | {:>8}",
        "Scenario",
        "Mean",
        "Min",
        "p50",
        "p99",
        "p99.9",
        "Max",
        width = SCENARIO_COL_WIDTH
    );
    println!("{}", "-".repeat(TABLE_WIDTH));
}

fn print_row(name: &str, stats: &LatencyStats) {
    println!(
        "{:<width$} | {:>8.1} | {:>8} | {:>8} | {:>8} | {:>8} | {:>8}",
        name,
        stats.mean,
        stats.min,
        stats.p50,
        stats.p99,
        stats.p99_9,
        stats.max,
        width = SCENARIO_COL_WIDTH
    );
}

fn print_row_r_channel_batch<F>(batch_pairs: &[(usize, usize)], lat_fun: F)
where
    F: Fn(usize, usize) -> LatencyStats,
{
    let mut stats = Vec::with_capacity(batch_pairs.len());
    for (p_batch, c_batch) in batch_pairs {
        let stat = lat_fun(*p_batch, *c_batch);
        stats.push((*p_batch, *c_batch, stat));
    }

    for (p_batch, c_batch, stat) in &stats {
        print_row(
            &format!(
                "enso_channel (p_batch={} c_batch={} full RTT)",
                p_batch, c_batch
            ),
            stat,
        );
    }
    println!("{}", "-".repeat(TABLE_WIDTH));
    for (p_batch, c_batch, stat) in stats {
        print_row(
            &format!(
                "enso_channel (p_batch={} c_batch={} amortized)",
                p_batch, c_batch
            ),
            &stat.amortized(c_batch),
        );
    }
}

// =============================================================================
// SPSC Latency Benchmarks
// =============================================================================

mod spsc {
    use super::*;

    fn rtt_measure<T, R, TF, RF>(
        mut tx1: T,
        mut tx2: T,
        mut rx1: R,
        mut rx2: R,
        mut send_func: TF,
        mut recv_func: RF,
    ) -> LatencyStats
    where
        TF: FnMut(&mut T, &Backoff) + Copy + Send + 'static,
        RF: FnMut(&mut R, &Backoff) + Copy + Send + 'static,
        T: Send + 'static,
        R: Send + 'static,
    {
        let (core0, core1) = get_core_ids();
        let barrier = Barrier::new(2);
        let barrier = std::sync::Arc::new(barrier);
        let mut samples = Vec::with_capacity(MEASURE_ITERS);

        let barrier_t1 = barrier.clone();
        let handle = thread::spawn(move || {
            pin_to_core(core1);
            barrier_t1.wait();

            let recv_backoff = Backoff::new();
            let send_backoff = Backoff::new();

            for _ in 0..(WARMUP_ITERS + MEASURE_ITERS) {
                recv_func(&mut rx1, &recv_backoff);
                send_func(&mut tx2, &send_backoff);
            }
        });

        pin_to_core(core0);
        barrier.wait();

        let main_send_backoff = Backoff::new();
        let main_recv_backoff = Backoff::new();

        for i in 0..(WARMUP_ITERS + MEASURE_ITERS) {
            let start = Instant::now();

            send_func(&mut tx1, &main_send_backoff);
            recv_func(&mut rx2, &main_recv_backoff);

            let elapsed = start.elapsed().as_nanos() as u64;
            if i >= WARMUP_ITERS {
                samples.push(elapsed);
            }
        }

        handle.join().unwrap();
        LatencyStats::from_samples(samples)
    }

    pub fn enso_channel_single() -> LatencyStats {
        let (tx1, rx1) = enso_channel::exclusive::spsc::channel::<u64>(BUFFER_SIZE);
        let (tx2, rx2) = enso_channel::exclusive::spsc::channel::<u64>(BUFFER_SIZE);
        rtt_measure(
            tx1,
            tx2,
            rx1,
            rx2,
            |tx, backoff| {
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(0u64)) {
                        Ok(_) => break,
                        Err(_) => backoff.spin(),
                    }
                }
            },
            |rx, backoff| {
                backoff.reset();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(v);
                            break;
                        }
                        Err(_) => backoff.spin(),
                    }
                }
            },
        )
    }

    pub fn enso_channel_batch(batch_size: usize) -> LatencyStats {
        let (tx1, rx1) = enso_channel::exclusive::spsc::channel::<u64>(BUFFER_SIZE);
        let (tx2, rx2) = enso_channel::exclusive::spsc::channel::<u64>(BUFFER_SIZE);
        rtt_measure(
            tx1,
            tx2,
            rx1,
            rx2,
            move |tx, backoff| {
                let mut sent = 0;
                while sent < batch_size {
                    backoff.reset();
                    loop {
                        if let Ok(mut batch) =
                            tx.try_send_many(batch_size - sent, || std::hint::black_box(0u64))
                        {
                            let n = batch.capacity();
                            for _ in 0..n {
                                batch.write_next(std::hint::black_box(0u64));
                            }
                            batch.finish();
                            sent += n;
                            break;
                        } else {
                            backoff.spin();
                        }
                    }
                }
            },
            move |rx, backoff| {
                let mut received = 0;
                while received < batch_size {
                    backoff.reset();
                    loop {
                        if let Ok(iter) = rx.try_recv_many(batch_size - received) {
                            for g in iter {
                                std::hint::black_box(*g);
                                received += 1;
                            }
                            break;
                        } else {
                            backoff.spin();
                        }
                    }
                }
            },
        )
    }

    pub fn crossbeam() -> LatencyStats {
        let (tx1, rx1) = crossbeam_channel::bounded::<u64>(BUFFER_SIZE);
        let (tx2, rx2) = crossbeam_channel::bounded::<u64>(BUFFER_SIZE);
        rtt_measure(
            tx1,
            tx2,
            rx1,
            rx2,
            |tx, backoff| {
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(0u64)) {
                        Ok(_) => break,
                        Err(_) => backoff.spin(),
                    }
                }
            },
            |rx, backoff| {
                backoff.reset();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(v);
                            break;
                        }
                        Err(_) => backoff.spin(),
                    }
                }
            },
        )
    }

    pub fn flume() -> LatencyStats {
        let (tx1, rx1) = flume::bounded::<u64>(BUFFER_SIZE);
        let (tx2, rx2) = flume::bounded::<u64>(BUFFER_SIZE);
        rtt_measure(
            tx1,
            tx2,
            rx1,
            rx2,
            |tx, backoff| {
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(0u64)) {
                        Ok(_) => break,
                        Err(_) => backoff.spin(),
                    }
                }
            },
            |rx, backoff| {
                backoff.reset();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(v);
                            break;
                        }
                        Err(_) => backoff.spin(),
                    }
                }
            },
        )
    }
}

// =============================================================================
// MPSC Latency Benchmarks (exclusive)
// =============================================================================

mod mpsc {

    use super::*;

    fn rtt_measure<T, R, TF, RF>(
        producer_count: usize,
        tx: T,
        mut rx: R,
        mut send_func: TF,
        mut recv_func: RF,
    ) -> LatencyStats
    where
        TF: FnMut(&mut T, usize, &Backoff) -> usize + Copy + Send + 'static,
        RF: FnMut(&mut R, &Backoff) -> usize + Copy + Send + 'static,
        T: Send + Clone + 'static,
        R: Send + 'static,
    {
        let (core0, core1) = get_core_ids();
        let barrier = Barrier::new(producer_count + 2); // producers + consumer + main
        let barrier = std::sync::Arc::new(barrier);
        let mut samples = Vec::with_capacity(MEASURE_ITERS);
        let mut handles = Vec::with_capacity(producer_count);

        let total_iters = (WARMUP_ITERS + MEASURE_ITERS) * producer_count;

        // Spawn producers
        for p in 0..producer_count {
            let mut tx = tx.clone();
            let barrier = barrier.clone();
            let handle = thread::spawn(move || {
                if p == 0 {
                    pin_to_core(core0);
                }
                barrier.wait();
                let mut sent = 0;
                let backoff = Backoff::new();
                while sent < WARMUP_ITERS + MEASURE_ITERS {
                    let left = (WARMUP_ITERS + MEASURE_ITERS) - sent;
                    let sent_now = send_func(&mut tx, left, &backoff);
                    sent += sent_now;
                }
            });
            handles.push(handle);
        }

        // Spawn consumer
        let barrier_consumer = barrier.clone();
        let consumer = thread::spawn(move || {
            pin_to_core(core1);
            barrier_consumer.wait();

            let mut total = 0;
            let backoff = Backoff::new();
            while total < total_iters {
                let start = Instant::now();
                let received = recv_func(&mut rx, &backoff);
                let elapsed = start.elapsed().as_nanos() as u64;
                total += received;

                if received > 0 && total >= WARMUP_ITERS * producer_count {
                    samples.push(elapsed);
                }
            }
            LatencyStats::from_samples(samples)
        });

        // Main thread starts the barrier
        barrier.wait();
        for h in handles {
            h.join().unwrap();
        }
        consumer.join().unwrap()
    }

    pub fn enso_channel_single(producer_count: usize) -> LatencyStats {
        let (tx, rx) = enso_channel::exclusive::mpsc::channel::<u64>(BUFFER_SIZE);
        rtt_measure(
            producer_count,
            tx,
            rx,
            |tx, _, backoff| {
                // let seq = seq.fetch_add(1, Ordering::Relaxed);
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(0u64)) {
                        Ok(_) => break 1,
                        Err(_) => backoff.spin(),
                    }
                }
            },
            |rx, backoff| {
                backoff.reset();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(v);
                            break 1;
                        }
                        Err(_) => backoff.spin(),
                    }
                }
            },
        )
    }

    pub fn enso_channel_batch(producer_count: usize, batch_size: usize) -> LatencyStats {
        let (tx, rx) = enso_channel::exclusive::mpsc::channel::<u64>(BUFFER_SIZE);
        rtt_measure(
            producer_count,
            tx,
            rx,
            move |tx, left, backoff| {
                let to_send = batch_size.min(left);
                backoff.reset();
                loop {
                    if let Ok(mut batch) = tx.try_send_many(to_send, || std::hint::black_box(0u64))
                    {
                        let n = batch.capacity();
                        for _ in 0..n {
                            batch.write_next(std::hint::black_box(0u64));
                        }
                        batch.finish();
                        break n;
                    } else {
                        backoff.spin();
                    }
                }
            },
            move |rx, backoff| {
                let mut to_recv = batch_size;
                let mut received = 0;
                loop {
                    match rx.try_recv_many(to_recv) {
                        Ok(iter) => {
                            backoff.reset();
                            for i in iter {
                                std::hint::black_box(*i);
                                received += 1;
                            }
                            if received > 0 {
                                break received;
                            }
                        }
                        Err(enso_channel::errors::TryRecvError::InsufficientItems { missing }) => {
                            let available = to_recv - missing;
                            if available > 0 {
                                to_recv = available;
                            } else {
                                backoff.spin();
                                to_recv = batch_size;
                            }
                        }
                        Err(_) => {
                            backoff.spin();
                            to_recv = batch_size;
                        }
                    }
                }
            },
        )
    }

    pub fn crossbeam(producer_count: usize) -> LatencyStats {
        let (tx, rx) = crossbeam_channel::bounded::<u64>(BUFFER_SIZE);
        rtt_measure(
            producer_count,
            tx,
            rx,
            |tx, _, backoff| {
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(0u64)) {
                        Ok(_) => break 1,
                        Err(_) => backoff.spin(),
                    }
                }
            },
            |rx, backoff| {
                backoff.reset();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(v);
                            break 1;
                        }
                        Err(_) => backoff.spin(),
                    }
                }
            },
        )
    }

    pub fn flume(producer_count: usize) -> LatencyStats {
        let (tx, rx) = flume::bounded::<u64>(BUFFER_SIZE);
        rtt_measure(
            producer_count,
            tx,
            rx,
            |tx, _, backoff| {
                backoff.reset();
                loop {
                    match tx.try_send(std::hint::black_box(0u64)) {
                        Ok(_) => break 1,
                        Err(_) => backoff.spin(),
                    }
                }
            },
            |rx, backoff| {
                backoff.reset();
                loop {
                    match rx.try_recv() {
                        Ok(v) => {
                            std::hint::black_box(v);
                            break 1;
                        }
                        Err(_) => backoff.spin(),
                    }
                }
            },
        )
    }
}

// =============================================================================
// Fanout (SPMC) Latency Benchmarks
// =============================================================================

mod fanout {
    use super::*;

    /// Measures latency from producer's perspective (time until all consumers have received)
    pub fn enso_channel_single<const N: usize>() -> LatencyStats {
        let (core0, core1) = get_core_ids();
        let (mut tx, rxs): (enso_channel::fanout::spmc::Sender<u64, N>, _) =
            enso_channel::fanout::spmc::channel(BUFFER_SIZE);

        let barrier = Barrier::new(N + 1);
        let barrier = std::sync::Arc::new(barrier);
        let ack_counter = AtomicU64::new(0);
        let ack_counter = std::sync::Arc::new(ack_counter);
        let mut samples = Vec::with_capacity(MEASURE_ITERS);
        let total_iters = WARMUP_ITERS + MEASURE_ITERS;

        let mut handles = Vec::new();
        for (i, mut rx) in rxs.into_iter().enumerate() {
            let barrier = barrier.clone();
            let ack_counter = ack_counter.clone();
            handles.push(thread::spawn(move || {
                if i == 0 {
                    pin_to_core(core1);
                }
                barrier.wait();

                let backoff = Backoff::new();
                for _ in 0..(WARMUP_ITERS + MEASURE_ITERS) {
                    backoff.reset();
                    loop {
                        match rx.try_recv() {
                            Ok(i) => {
                                std::hint::black_box(*i);
                                ack_counter.fetch_add(1, Ordering::Release);
                                break;
                            }
                            Err(enso_channel::errors::TryRecvError::Disconnected) => return,
                            Err(_) => backoff.spin(),
                        }
                    }
                }
            }));
        }

        pin_to_core(core0);
        barrier.wait();

        let send_backoff = Backoff::new();
        let ack_backoff = Backoff::new();
        for i in 0..total_iters {
            let expected_acks = ((i + 1) * N) as u64;
            let start = Instant::now();

            // Send
            send_backoff.reset();
            loop {
                match tx.try_send(std::hint::black_box(i as u64)) {
                    Ok(_) => break,
                    Err(_) => send_backoff.spin(),
                }
            }

            // Wait for all consumers to ack
            ack_backoff.reset();
            while ack_counter.load(Ordering::Acquire) < expected_acks {
                ack_backoff.spin();
            }

            let elapsed = start.elapsed().as_nanos() as u64;

            if i >= WARMUP_ITERS {
                samples.push(elapsed);
            }
        }

        drop(tx);
        for h in handles {
            h.join().unwrap();
        }

        LatencyStats::from_samples(samples)
    }

    pub fn enso_channel_batch<const N: usize>(batch_size: usize) -> LatencyStats {
        let (core0, core1) = get_core_ids();
        let (mut tx, rxs): (enso_channel::fanout::spmc::Sender<u64, N>, _) =
            enso_channel::fanout::spmc::channel(BUFFER_SIZE);

        let barrier = Barrier::new(N + 1);
        let barrier = std::sync::Arc::new(barrier);
        let ack_counter = AtomicU64::new(0);
        let ack_counter = std::sync::Arc::new(ack_counter);
        let mut samples = Vec::with_capacity(MEASURE_ITERS / batch_size + 1);

        let total_iters = WARMUP_ITERS + MEASURE_ITERS;

        let mut handles = Vec::new();
        for (i, mut rx) in rxs.into_iter().enumerate() {
            let barrier = barrier.clone();
            let ack_counter = ack_counter.clone();
            handles.push(thread::spawn(move || {
                if i == 0 {
                    pin_to_core(core1);
                }
                barrier.wait();

                let mut received = 0;
                let backoff = Backoff::new();
                while received < total_iters {
                    backoff.reset();
                    let to_recv = batch_size.min(total_iters - received);
                    if let Ok(iter) = rx.try_recv_many(to_recv) {
                        let mut count = 0;
                        for i in iter {
                            std::hint::black_box(*i);
                            count += 1;
                        }
                        received += count;
                        ack_counter.fetch_add(count as u64, Ordering::Release);
                    } else {
                        backoff.spin();
                    }
                }
            }));
        }

        pin_to_core(core0);
        barrier.wait();

        let mut sent = 0;

        let send_backoff = Backoff::new();
        let ack_backoff = Backoff::new();
        while sent < total_iters {
            let to_send = batch_size.min(total_iters - sent);
            let expected_acks = ((sent + to_send) * N) as u64;
            let start = Instant::now();

            // Send batch
            send_backoff.reset();
            loop {
                if let Ok(mut batch) = tx.try_send_many(to_send, || std::hint::black_box(0u64)) {
                    let n = batch.capacity();
                    for _ in 0..n {
                        batch.write_next(std::hint::black_box(0u64));
                    }
                    batch.finish();
                    sent += n;
                    break;
                } else {
                    send_backoff.spin();
                }
            }

            // Wait for all consumers to ack
            ack_backoff.reset();
            while ack_counter.load(Ordering::Acquire) < expected_acks {
                ack_backoff.spin();
            }

            let elapsed = start.elapsed().as_nanos() as u64;

            if sent > WARMUP_ITERS {
                samples.push(elapsed); // Full batch latency
            }
        }

        drop(tx);
        for h in handles {
            h.join().unwrap();
        }

        LatencyStats::from_samples(samples)
    }
}

// =============================================================================
// Work Queue (MPMC) Latency Benchmarks
// =============================================================================

mod queue {
    use super::*;

    /// Message with embedded timestamp for one-way latency measurement.
    #[derive(Copy, Clone)]
    struct TimedMsg {
        sent_at: Instant,
        payload: u64,
    }

    impl Default for TimedMsg {
        fn default() -> Self {
            Self {
                sent_at: Instant::now(),
                payload: 0,
            }
        }
    }

    /// Helper to distribute messages evenly across producers.
    fn messages_for_producer(p: usize, producer_count: usize, total: usize) -> usize {
        let base = total / producer_count;
        let rem = total % producer_count;
        base + usize::from(p < rem)
    }

    /// Work queue one-way latency: measures per-message send→recv time.
    /// Each message carries a timestamp; consumer computes elapsed since send.
    pub fn enso_channel_single(producer_count: usize, consumer_count: usize) -> LatencyStats {
        let (core0, core1) = get_core_ids();
        let (tx, rx) = enso_channel::queue::mpmc::channel::<TimedMsg>(BUFFER_SIZE);
        let barrier = std::sync::Arc::new(Barrier::new(producer_count + consumer_count + 1));

        let total_messages = WARMUP_ITERS + MEASURE_ITERS;
        let recv_counter = std::sync::Arc::new(AtomicU64::new(0));

        // Producers: stamp each message at successful enqueue
        let mut producer_handles = Vec::with_capacity(producer_count);
        for p in 0..producer_count {
            let mut tx = tx.clone();
            let barrier = barrier.clone();
            let to_send = messages_for_producer(p, producer_count, total_messages);

            producer_handles.push(thread::spawn(move || {
                if p == 0 {
                    pin_to_core(core0);
                }
                barrier.wait();

                let backoff = Backoff::new();
                for i in 0..to_send {
                    backoff.reset();
                    loop {
                        // Timestamp created per attempt; only the successful one matters
                        let msg = TimedMsg {
                            sent_at: Instant::now(),
                            payload: i as u64,
                        };
                        match tx.try_send(std::hint::black_box(msg)) {
                            Ok(_) => break,
                            Err(_) => {
                                backoff.spin();
                            }
                        }
                    }
                }
            }));
        }

        // Consumers: thread-local samples (no mutex in hot path)
        let mut consumer_handles = Vec::with_capacity(consumer_count);
        for c in 0..consumer_count {
            let mut rx = rx.clone();
            let barrier = barrier.clone();
            let recv_counter = recv_counter.clone();

            consumer_handles.push(thread::spawn(move || {
                if c == 0 {
                    pin_to_core(core1);
                }
                barrier.wait();

                let mut samples =
                    Vec::with_capacity(MEASURE_ITERS.saturating_div(consumer_count).max(1));

                let backoff = Backoff::new();
                loop {
                    if recv_counter.load(Ordering::Relaxed) >= total_messages as u64 {
                        return samples;
                    }

                    match rx.try_recv() {
                        Ok(guard) => {
                            let now = Instant::now();
                            backoff.reset();
                            let msg = *guard;
                            std::hint::black_box(msg.payload);

                            let elapsed = now.duration_since(msg.sent_at).as_nanos() as u64;
                            let count = recv_counter.fetch_add(1, Ordering::Relaxed) + 1;

                            if count > WARMUP_ITERS as u64 {
                                samples.push(elapsed);
                            }
                        }
                        Err(enso_channel::errors::TryRecvError::Disconnected) => {
                            return samples;
                        }
                        Err(_) => {
                            if recv_counter.load(Ordering::Relaxed) >= total_messages as u64 {
                                return samples;
                            }
                            backoff.spin();
                        }
                    }
                }
            }));
        }

        barrier.wait();

        for h in producer_handles {
            h.join().expect("producer panicked");
        }

        let mut all_samples = Vec::with_capacity(MEASURE_ITERS);
        for h in consumer_handles {
            all_samples.extend(h.join().expect("consumer panicked"));
        }

        LatencyStats::from_samples(all_samples)
    }

    /// Crossbeam one-way latency for fair comparison.
    pub fn crossbeam(producer_count: usize, consumer_count: usize) -> LatencyStats {
        let (core0, core1) = get_core_ids();
        let (tx, rx) = crossbeam_channel::bounded::<TimedMsg>(BUFFER_SIZE);
        let barrier = std::sync::Arc::new(Barrier::new(producer_count + consumer_count + 1));

        let total_messages = WARMUP_ITERS + MEASURE_ITERS;
        let recv_counter = std::sync::Arc::new(AtomicU64::new(0));

        // Producers
        let mut producer_handles = Vec::with_capacity(producer_count);
        for p in 0..producer_count {
            let tx = tx.clone();
            let barrier = barrier.clone();
            let to_send = messages_for_producer(p, producer_count, total_messages);

            producer_handles.push(thread::spawn(move || {
                if p == 0 {
                    pin_to_core(core0);
                }
                barrier.wait();

                let backoff = Backoff::new();
                for i in 0..to_send {
                    backoff.reset();
                    loop {
                        let msg = TimedMsg {
                            sent_at: Instant::now(),
                            payload: i as u64,
                        };
                        match tx.try_send(std::hint::black_box(msg)) {
                            Ok(_) => break,
                            Err(_) => {
                                backoff.spin();
                            }
                        }
                    }
                }
            }));
        }

        // Consumers
        let mut consumer_handles = Vec::with_capacity(consumer_count);
        for c in 0..consumer_count {
            let rx = rx.clone();
            let barrier = barrier.clone();
            let recv_counter = recv_counter.clone();

            consumer_handles.push(thread::spawn(move || {
                if c == 0 {
                    pin_to_core(core1);
                }
                barrier.wait();

                let mut samples =
                    Vec::with_capacity(MEASURE_ITERS.saturating_div(consumer_count).max(1));

                let backoff = Backoff::new();
                loop {
                    if recv_counter.load(Ordering::Relaxed) >= total_messages as u64 {
                        return samples;
                    }

                    match rx.try_recv() {
                        Ok(msg) => {
                            let now = Instant::now();
                            backoff.reset();
                            std::hint::black_box(msg.payload);

                            let elapsed = now.duration_since(msg.sent_at).as_nanos() as u64;
                            let count = recv_counter.fetch_add(1, Ordering::Relaxed) + 1;

                            if count > WARMUP_ITERS as u64 {
                                samples.push(elapsed);
                            }
                        }
                        Err(crossbeam_channel::TryRecvError::Disconnected) => return samples,
                        Err(_) => {
                            if recv_counter.load(Ordering::Relaxed) >= total_messages as u64 {
                                return samples;
                            }
                            backoff.spin();
                        }
                    }
                }
            }));
        }

        barrier.wait();

        for h in producer_handles {
            h.join().expect("producer panicked");
        }

        let mut all_samples = Vec::with_capacity(MEASURE_ITERS);
        for h in consumer_handles {
            all_samples.extend(h.join().expect("consumer panicked"));
        }

        LatencyStats::from_samples(all_samples)
    }

    /// Work queue batch one-way latency: per-message send→recv time using batch APIs.
    /// Consumer takes one `Instant::now()` per batch and computes latency for each message.
    pub fn enso_channel_batch(
        producer_count: usize,
        consumer_count: usize,
        producer_batch_size: usize,
        consumer_batch_size: usize,
    ) -> LatencyStats {
        let (core0, core1) = get_core_ids();
        let (tx, rx) = enso_channel::queue::mpmc::channel::<TimedMsg>(BUFFER_SIZE);
        let barrier = std::sync::Arc::new(Barrier::new(producer_count + consumer_count + 1));

        let total_messages = WARMUP_ITERS + MEASURE_ITERS;
        let recv_counter = std::sync::Arc::new(AtomicU64::new(0));

        // Producers: batch send with per-message timestamp (instrumented)
        let prod_block_ns_batch = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let prod_block_count_batch = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut producer_handles = Vec::with_capacity(producer_count);
        for p in 0..producer_count {
            let mut tx = tx.clone();
            let barrier = barrier.clone();
            let to_send = messages_for_producer(p, producer_count, total_messages);
            let prod_block_ns_batch = prod_block_ns_batch.clone();
            let prod_block_count_batch = prod_block_count_batch.clone();

            producer_handles.push(thread::spawn(move || {
                if p == 0 {
                    pin_to_core(core0);
                }
                barrier.wait();

                let mut sent = 0;
                let backoff = Backoff::new();
                while sent < to_send {
                    let mut n_target = producer_batch_size.min(to_send - sent);
                    // Instrument the try_send_many loop
                    let start_try = Instant::now();
                    let mut attempts = 0u64;
                    loop {
                        attempts += 1;
                        // Use a placeholder init; we overwrite with write_next anyway
                        let init_ts = Instant::now();
                        if let Ok(mut batch) = tx.try_send_many(n_target, || {
                            std::hint::black_box(TimedMsg {
                                sent_at: init_ts,
                                payload: 0,
                            })
                        }) {
                            let n = batch.capacity();
                            backoff.reset();
                            for i in 0..n {
                                let now = Instant::now();
                                batch.write_next(std::hint::black_box(TimedMsg {
                                    sent_at: now,
                                    payload: (sent + i) as u64,
                                }));
                            }
                            batch.finish();
                            sent += n;
                            break;
                        } else {
                            // Reduce target to make progress under contention
                            if n_target > 1 {
                                n_target = (n_target / 2).max(1);
                            } else {
                                backoff.spin();
                            }
                        }
                    }
                    let blocked = start_try.elapsed().as_nanos() as u64;
                    if attempts > 1 {
                        prod_block_ns_batch.fetch_add(blocked, Ordering::Relaxed);
                        prod_block_count_batch.fetch_add(attempts - 1, Ordering::Relaxed);
                    }
                }
            }));
        }

        // Consumers: batch recv, one now() per batch, compute per-message latency (instrumented)
        let cons_block_ns_batch = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cons_block_count_batch = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut consumer_handles = Vec::with_capacity(consumer_count);
        for c in 0..consumer_count {
            let mut rx = rx.clone();
            let barrier = barrier.clone();
            let recv_counter = recv_counter.clone();
            let cons_block_ns_batch = cons_block_ns_batch.clone();
            let cons_block_count_batch = cons_block_count_batch.clone();

            consumer_handles.push(thread::spawn(move || {
                if c == 0 {
                    pin_to_core(core1);
                }
                barrier.wait();

                let mut samples =
                    Vec::with_capacity(MEASURE_ITERS.saturating_div(consumer_count).max(1));

                let mut to_recv = consumer_batch_size;
                let backoff = crossbeam_utils::Backoff::new();
                loop {
                    let total_recv = recv_counter.load(Ordering::Relaxed);
                    if total_recv >= total_messages as u64 {
                        return samples;
                    }
                    to_recv = to_recv.min(total_messages - total_recv as usize);

                    let start_try = Instant::now();
                    match rx.try_recv_many(to_recv) {
                        Ok(iter) => {
                            let now = Instant::now();
                            backoff.reset();

                            for guard in iter {
                                let msg = *guard;
                                std::hint::black_box(msg.payload);

                                let elapsed = now.duration_since(msg.sent_at).as_nanos() as u64;
                                let count = recv_counter.fetch_add(1, Ordering::Relaxed) + 1;

                                if count > WARMUP_ITERS as u64 {
                                    samples.push(elapsed);
                                }

                                if count >= total_messages as u64 {
                                    return samples;
                                }
                            }
                        }
                        Err(enso_channel::errors::TryRecvError::InsufficientItems { .. }) => {
                            let blocked = start_try.elapsed().as_nanos() as u64;
                            cons_block_ns_batch.fetch_add(blocked, Ordering::Relaxed);
                            cons_block_count_batch.fetch_add(1, Ordering::Relaxed);

                            backoff.spin();
                        }
                        Err(enso_channel::errors::TryRecvError::Disconnected) => {
                            return samples;
                        }
                    }
                }
            }));
        }

        barrier.wait();

        for h in producer_handles {
            h.join().expect("producer panicked");
        }

        let mut all_samples = Vec::with_capacity(MEASURE_ITERS);
        for h in consumer_handles {
            all_samples.extend(h.join().expect("consumer panicked"));
        }

        // Diagnostic print showing blocked ns/event counts per side when enabled
        if std::env::var("ENSO_CHANNEL_QUEUE_DIAG").is_ok() {
            let prod_ns = prod_block_ns_batch.load(Ordering::Relaxed);
            let prod_events = prod_block_count_batch.load(Ordering::Relaxed);
            let cons_ns = cons_block_ns_batch.load(Ordering::Relaxed);
            let cons_events = cons_block_count_batch.load(Ordering::Relaxed);
            println!(
                "WorkQueue diag (p={}, c={}, p_batch={}, c_batch={}): producer_block_ns={}, producer_block_events={}, consumer_block_ns={}, consumer_block_events={}",
                producer_count,
                consumer_count,
                producer_batch_size,
                consumer_batch_size,
                prod_ns,
                prod_events,
                cons_ns,
                cons_events,
            );
        }

        LatencyStats::from_samples(all_samples)
    }
}

// =============================================================================
// Main
// =============================================================================

fn main() {
    println!("enso_channel Tail Latency Benchmark");
    println!("==================================");
    println!(
        "Warmup: {} iterations, Measure: {} iterations, buffer size: {}",
        WARMUP_ITERS, MEASURE_ITERS, BUFFER_SIZE
    );
    println!("All values in nanoseconds (ns)");

    let (core0, core1) = get_core_ids();
    if core0 == core1 && core0.id == 0 {
        println!("WARNING: Pinning is not available on this platform!");
    }

    let batch_sizes = [4, 8, 16, 32, 64];

    // SPSC
    print_section("SPSC (Single-Producer Single-Consumer)");
    print_row("enso_channel (single)", &spsc::enso_channel_single());
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |_, batch| spsc::enso_channel_batch(batch));
    print_row("crossbeam", &spsc::crossbeam());
    print_row("flume", &spsc::flume());

    // MPSC
    print_section("MPSC (Multi-Producer Single-Consumer, 2 producers)");
    print_row("enso_channel (single)", &mpsc::enso_channel_single(2));
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |_, batch| mpsc::enso_channel_batch(2, batch));
    print_row("crossbeam", &mpsc::crossbeam(2));
    print_row("flume", &mpsc::flume(2));

    // MPSC
    print_section("MPSC (Multi-Producer Single-Consumer, 4 producers)");
    print_row("enso_channel (single)", &mpsc::enso_channel_single(4));
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |_, batch| mpsc::enso_channel_batch(4, batch));
    print_row("crossbeam", &mpsc::crossbeam(4));
    print_row("flume", &mpsc::flume(4));

    // Fanout SPMC
    print_section("Fanout SPMC (1 producer, 2 consumers)");
    print_row("enso_channel (single)", &fanout::enso_channel_single::<2>());
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |_, b| fanout::enso_channel_batch::<2>(b));

    print_section("Fanout SPMC (1 producer, 4 consumers)");
    print_row("enso_channel (single)", &fanout::enso_channel_single::<4>());
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |_, b| fanout::enso_channel_batch::<4>(b));

    // Work Queue
    print_section("Work Queue MPMC (1 producers, 1 consumers)");
    print_row("enso_channel (single)", &queue::enso_channel_single(1, 1));
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |p, c| queue::enso_channel_batch(1, 1, p, c));
    print_row("crossbeam", &queue::crossbeam(1, 1));

    print_section("Work Queue MPMC (1 producers, 4 consumers)");
    print_row("enso_channel (single)", &queue::enso_channel_single(1, 4));
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |p, c| queue::enso_channel_batch(1, 4, p, c));
    print_row("crossbeam", &queue::crossbeam(1, 4));

    print_section("Work Queue MPMC (2 producers, 2 consumers)");
    print_row("enso_channel (single)", &queue::enso_channel_single(2, 2));
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |p, c| queue::enso_channel_batch(2, 2, p, c));
    print_row("crossbeam", &queue::crossbeam(2, 2));

    print_section("Work Queue MPMC (4 producers, 2 consumers)");
    print_row("enso_channel (single)", &queue::enso_channel_single(4, 2));
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |p, c| queue::enso_channel_batch(4, 2, p, c));
    print_row("crossbeam", &queue::crossbeam(4, 2));

    print_section("Work Queue MPMC (6 producers, 2 consumers)");
    print_row("enso_channel (single)", &queue::enso_channel_single(6, 2));
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |p, c| queue::enso_channel_batch(6, 2, p, c));
    print_row("crossbeam", &queue::crossbeam(6, 2));

    print_section("Work Queue MPMC (4 producers, 4 consumers)");
    print_row("enso_channel (single)", &queue::enso_channel_single(4, 4));
    let batch_pairs: Vec<(usize, usize)> = batch_sizes.iter().map(|b| (*b, *b)).collect();
    print_row_r_channel_batch(&batch_pairs, |p, c| queue::enso_channel_batch(4, 4, p, c));
    print_row("crossbeam", &queue::crossbeam(4, 4));

    print_section("Work Queue MPMC (2 producers, 2 consumers) varying batch sizes");
    let batch_pairs: Vec<(usize, usize)> = vec![(4, 16), (4, 32), (8, 16), (8, 32), (8, 64)];
    print_row_r_channel_batch(&batch_pairs, |p, c| queue::enso_channel_batch(2, 2, p, c));

    print_section("Work Queue MPMC (2 producers, 4 consumers) varying batch sizes");
    // let batch_pairs: Vec<(usize, usize)> = vec![(8, 16), (8, 32), (8, 64), (16, 16), (32, 16)];
    let batch_pairs: Vec<(usize, usize)> = vec![(4, 16), (4, 32), (8, 16), (8, 32), (8, 64)];
    print_row_r_channel_batch(&batch_pairs, |p, c| queue::enso_channel_batch(2, 4, p, c));

    println!("\n Legend:");
    println!("  - SPSC/MPSC/Fanout 'full RTT': Total batch round-trip time");
    println!("  - SPSC/MPSC/Fanout 'amortized': Per-item RTT (full RTT / batch_size)");
    println!("  - Work Queue: One-way send→recv latency per message (not RTT)");
}
