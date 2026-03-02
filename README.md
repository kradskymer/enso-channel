<div align="center">

# enso-channel

<img src="enso_channel_logo.png" alt="enso-channel logo" height="300" width="300">

### **Bounded. Lock-free. Batch-native.**

*A batch-first concurrency primitive for bursty, latency-sensitive systems.*

[![Crates.io](https://img.shields.io/crates/v/enso-channel.svg)](https://crates.io/crates/enso-channel)
[![Documentation](https://docs.rs/enso-channel/badge.svg)](https://docs.rs/enso-channel)
[![License](https://img.shields.io/crates/l/enso-channel.svg)](LICENSE-MIT)

</div>

---

## Table of Contents

- [What is enso-channel?](#what-is-enso-channel)
- [Design Philosophy](#design-philosophy)
- [Non-Goals](#non-goals)
- [Communication Patterns](#communication-patterns)
- [System-Wide Tunability](#system-wide-tunability)
- [Getting Started](#getting-started)
- [Disconnection (RAII)](#disconnection-raii)
- [Performance Characteristics](#performance-characteristics)
- [Benchmark Overview](#benchmark-overview)
- [Correctness & Testing](#correctness--testing)
- [When to Use enso-channel](#when-to-use-enso-channel)
- [Roadmap](#roadmap)
- [License](#license)

---

## What is enso-channel?

`enso-channel` explores a **batch-first design space** for lock-free ring-buffer channels in Rust.

### Built for systems where

- Burst traffic is common
- Tail latency (p99 / p99.9) matters
- Backpressure must be explicit
- Memory is bounded and pre-allocated
- Scheduling is handled at a higher layer

> **Note:** If you need async/await integration, blocking APIs, or dynamic resizing — this crate is probably not for you.

---

## Design Philosophy

`enso-channel` is a **concurrency primitive**, not a runtime.

### Core Principles

- **Lock-free**
- **Bounded capacity**
- **Pre-allocated memory**
- **Explicit backpressure**
- **Native batch claim/commit**
- **No hidden scheduling**
- **No implicit blocking**

### Mental Model

Instead of sending items one-by-one:

```text
1. Claim a contiguous range in the ring buffer
2. Write into it / Read from it
3. Commit the range
```

**Batching amortizes synchronization cost and reduces tail latency under burst workloads.**

#### Inspired by

- [LMAX Exchange's Disruptor](https://github.com/LMAX-Exchange/disruptor)
- [crossbeam-channel](https://github.com/crossbeam-rs/crossbeam)

#### But with

- Rust-native channel ergonomics
- Unified API across topologies
- Clear primitive boundaries

---

## Non-Goals

> To prevent misuse and ambiguity:

- No blocking API
- No async integration
- No dynamic resizing
- No built-in scheduling policy
- No priority queues
- No fairness guarantees beyond lock-free progress

**Philosophy:** Scheduling, wake-up strategy, budgeting, priority, and flow control belong to higher layers.

`enso-channel` intentionally surfaces `Full`, `Empty`, and `Disconnected` instead of hiding them behind blocking semantics.

---

## Communication Patterns

Unified API across multiple topologies:

| Pattern       | Description                         |
| ------------- | ----------------------------------- |
| **SPSC**      | Single Producer, Single Consumer    |
| **MPSC**      | Multiple Producers, Single Consumer |
| **Broadcast** | Lossless Broadcast (fixed‑N fanout) |
| **MPMC**      | Work Distribution                   |

### Key Properties

- Receivers may initiate disconnect
- MPMC receivers can be cloned
- Broadcast topology is fixed at creation
- Disconnection follows RAII semantics
- All patterns support batch operations

---

## System-Wide Tunability

Tunability in `enso-channel` is not limited to a specific topology.

It emerges from its **batch-native design**.

### Every pattern supports

| API                                     | Description              |
| --------------------------------------- | ------------------------ |
| `try_send_many` / `try_recv_many`       | Batch claiming           |
| `try_send_at_most` / `try_recv_at_most` | At-most semantics        |
| Explicit backpressure                   | Surface `Full` / `Empty` |

### This enables systematic control over

- **Contention levels**
- **Cache locality**
- **Burst amortization**
- **Producer/consumer balance**
- **Tail latency behavior**

> **Note:** Under high contention (e.g. MPMC), the effect is especially visible — but tunability is inherent to the design, not limited to one pattern.

---

## Getting Started

### Installation

Add to your project:

```sh
cargo add enso-channel
```

### Example (MPSC)

```rust
use enso_channel::mpsc;

fn main() {
    let (mut tx, mut rx) = mpsc::channel::<u64>(64);

    // Single send/recv
    tx.try_send(42).unwrap();
    {
        let guard = rx.try_recv().unwrap();
        assert_eq!(*guard, 42);
    }

    // Batch send
    let mut batch = tx.try_send_many_default(8).unwrap();
    for i in 1..=8 {
        batch.write_next(i);
    }
    batch.finish();

    // Batch receive
    let iter = rx.try_recv_many(8).unwrap();
    for v in iter {
        println!("{v}");
    }
}
```

**All topologies share a consistent API shape.**

See [documentation](https://docs.rs/enso-channel) for additional examples.

---

## Disconnection (RAII)

`enso-channel` does **not** expose an explicit `close()` API.

### Disconnection follows RAII

- Dropping the last sender disconnects receivers (after draining committed items)
- Dropping the last receiver disconnects senders
- Batch guards commit automatically on drop

### Important Caveat

> **Disconnection is eventual, not transactional.**

A sender may still successfully publish if a receiver is dropped concurrently. Already-committed items may never be observed by the application.

#### Graceful shutdown without loss

```text
1. Stop producing
2. Drop all senders
3. Drain on the receiver until `Disconnected`
```

---

## Performance Characteristics

### Optimized for

- Burst latency
- Tail behavior (p99 / p99.9)
- Busy-spin workloads
- Deterministic bounded pipelines

### Not optimized for

- Blocking workloads
- Async runtimes
- Unbounded queues
- General-purpose task scheduling

---

## Benchmark Overview

### Benchmarks focus on

- **ns per burst**
- Mean and p99.9
- Busy-spin `try_*` loops
- Pinned threads (configurable)
- Fixed buffer sizes

> These measurements reflect burst-latency workloads under contention. They are not intended as general-purpose channel comparisons.

The numbers below are a selected snapshot to illustrate burst-latency behavior.

#### Notes

- All benches in this section report **ns/burst** (lower is better)
- The results are from a specific run on a MacBook Pro M4 (24G); your mileage may vary
- `broadcast` measures a burst as complete when it is delivered to **all** consumers
- These benches pin threads by default (set `ENSO_CHANNEL_PINNING=off` to disable)
- Outputs are printed to stdout and also written as CSV + table to a local output dir (default: `benches/results`, generated and gitignored). Override with `ENSO_SPSC_OUTPUT_DIR` / `ENSO_MPSC_OUTPUT_DIR` / `ENSO_MPMC_OUTPUT_DIR` / `ENSO_BROADCAST_OUTPUT_DIR`

### To reproduce

```sh
cargo bench --bench spsc
cargo bench --bench mpsc
cargo bench --bench mpmc
cargo bench --bench broadcast
```

For additional workloads/topologies, run the other bench targets under `benches/`.

---

### SPSC (ns/burst)

| burst | enso mean | enso p99.9 | crossbeam mean | crossbeam p99.9 |
| ----- | --------: | ---------: | -------------: | --------------: |
| 1     |     193.6 |        875 |          280.2 |             834 |
| 16    |     250.1 |        792 |          578.8 |            2835 |
| 64    |     357.0 |        792 |         1157.7 |            9047 |
| 128   |     495.4 |       1625 |         2271.0 |           17967 |

---

### MPSC (ns/burst)

#### 2 producers

| burst | enso mean | enso p99.9 | crossbeam mean | crossbeam p99.9 |
| ----- | --------: | ---------: | -------------: | --------------: |
| 1     |     673.1 |       2209 |         1022.6 |            7711 |
| 16    |     988.3 |       7919 |         2735.3 |           15007 |
| 64    |    1161.2 |       7875 |         4514.5 |           19087 |
| 128   |    1333.4 |      12839 |         6518.1 |           36319 |

#### 4 producers

| burst | enso mean | enso p99.9 | crossbeam mean | crossbeam p99.9 |
| ----- | --------: | ---------: | -------------: | --------------: |
| 1     |    1272.5 |      10711 |         1274.1 |            9215 |
| 16    |    1273.0 |       9007 |         7650.7 |           28431 |
| 64    |    1690.1 |      11711 |        21274.8 |           49919 |
| 128   |    2305.5 |      16295 |        35566.1 |           81663 |

---

### Work queue / MPMC tunability (ns/burst)

This bench measures **work distribution** (MPMC): total messages per burst = `burst * producers`,
and the burst is complete when the set is consumed by the worker pool.

For `enso_channel::mpmc`, we report the best `try_send_at_most` / `try_recv_at_most` tuning
**by lowest p99.9** among the tested `(send_limit, recv_limit)` pairs.

#### P2C2

| burst | enso tuning (send_limit, recv_limit) | enso mean | enso p99.9 | crossbeam mean | crossbeam p99.9 |
| ----- | -----------------------------------: | --------: | ---------: | -------------: | --------------: |
| 1     |                              (8, 64) |    1396.8 |       3001 |         1373.5 |            3251 |
| 16    |                              (8, 32) |    1553.1 |       2501 |         3502.0 |            9631 |
| 32    |                              (8, 32) |    1866.0 |       3833 |         4228.3 |           13959 |
| 64    |                              (8, 32) |    2887.7 |       6375 |         6074.2 |           19295 |

#### P4C4

| burst | enso tuning (send_limit, recv_limit) | enso mean | enso p99.9 | crossbeam mean | crossbeam p99.9 |
| ----- | -----------------------------------: | --------: | ---------: | -------------: | --------------: |
| 1     |                              (4, 32) |    1835.6 |      10295 |         2037.1 |           10591 |
| 16    |                              (8, 64) |    2453.8 |      10215 |        11610.8 |          127679 |
| 32    |                              (8, 32) |    4171.4 |      12463 |        15574.9 |          360703 |
| 64    |                              (8, 64) |    7390.4 |      15503 |        20208.5 |          598527 |

---

### Broadcast / SPMC scalability (ns/burst)

This bench reports **ns/burst**: one producer publishes a burst,
and the burst is complete when **all** consumers have received it.

| burst | C=2 mean | C=2 p99.9 | C=4 mean | C=4 p99.9 |
| ----- | -------: | --------: | -------: | --------: |
| 1     |    418.9 |      3417 |    751.1 |      1334 |
| 16    |    379.6 |      1334 |    756.7 |      4251 |
| 64    |    431.2 |      1334 |    723.3 |      1500 |
| 128   |    414.9 |      1417 |    817.1 |      1708 |

---

## Correctness & Testing

- **Miri clean**
- Tested on ARM (Apple Silicon) and x86_64
- Memory ordering documented in source
- Loom integration planned / in progress

> **Lock-free correctness and memory ordering are treated as first-class concerns.**

---

## When to Use enso-channel

### Use it if

- You control your scheduling model
- You prefer explicit backpressure
- Your workload is burst-heavy
- Tail latency matters
- You want deterministic bounded behavior

### Avoid it if

- You need async/await integration
- You want blocking `recv()`
- You prefer runtime-managed scheduling
- You require dynamic capacity growth

---

## Roadmap

- [ ] Loom verification
- [ ] Additional workload benchmarks
- [ ] More usage examples
- [ ] Documentation expansion

> **Note:** No plans for async or blocking APIs.

---

## License

Licensed under either:

- Apache License, Version 2.0
- MIT License
