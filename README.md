<div align="center">

# enso-channel

<img src="https://raw.githubusercontent.com/kradskymer/enso-channel/refs/heads/main/enso_channel_logo.png" alt="enso-channel logo" height="300" width="300">

### **Bounded. Lock-free. Batch-native. Async-aware.**

*A batch-first concurrency primitive for bursty, latency-sensitive systems.*

[![Crates.io](https://img.shields.io/crates/v/enso-channel.svg)](https://crates.io/crates/enso-channel)
[![Documentation](https://docs.rs/enso-channel/badge.svg)](https://docs.rs/enso-channel)
[![License](https://img.shields.io/crates/l/enso-channel.svg)](LICENSE-MIT)

</div>

---

## Table of Contents

- [What is enso-channel?](#what-is-enso-channel)
- [Design Philosophy](#design-philosophy)
- [Misuse prevention (compile-time)](#misuse-prevention-compile-time)
- [Non-goals](#non-goals)
- [Async model (receiver only)](#async-model-receiver-only)
- [Communication Patterns](#communication-patterns)
- [System-Wide Tunability](#system-wide-tunability)
- [Getting Started](#getting-started)
- [Lifecycle / shutdown (RAII)](#lifecycle--shutdown-raii)
- [Performance Characteristics](#performance-characteristics)
- [Benchmark Overview](#benchmark-overview)
- [Correctness & Testing](#correctness--testing)
- [Roadmap](#roadmap)
- [License](#license)

---

<!-- cargo-sync-rdme rustdoc [[ -->
### What is enso-channel?

`enso_channel` is a batch-first concurrency primitive: a family of bounded, lock-free,
ring-buffer channels designed for bursty, latency-sensitive systems.

`enso-channel` explores a **batch-first design space** for lock-free ring-buffer channels
in Rust.

The API is intentionally non-blocking: operations are exposed as `try_*` and surface
backpressure/termination explicitly via errors (`Full`, `Empty`, `Disconnected`).

#### Built for systems where

* Burst traffic is common
* Tail latency (p99 / p99.9) matters
* Back pressure must be explicit
* Memory is bounded and pre-allocated
* Scheduling is handled at a higher layer

 > 
 > **Note:** If you need blocking APIs, dynamic resizing, or async senders — this crate is
 > probably not for you. Receivers support async via an opt-in feature flag
 > (`async-receiver`).

### Design Philosophy

`enso-channel` is a **concurrency primitive**, not a runtime.

#### Core Principles

* **Lock-free**
* **Bounded capacity**
* **Pre-allocated memory**
* **Explicit back pressure**
* **Native batch claim/commit**
* **No hidden scheduling**
* **No implicit blocking**
* **Asymmetric async (receiver only, opt-in)**

#### Mental Model

Instead of sending / receiving items one-by-one:

````text
1. Claim a contiguous range in the ring buffer
2. Write into it / Read from it
3. Commit the range
````

**Batching amortizes synchronization cost and reduces tail latency under burst workloads.**

##### Inspired by

* [LMAX Exchange’s Disruptor](https://github.com/LMAX-Exchange/disruptor)
* [crossbeam-channel](https://github.com/crossbeam-rs/crossbeam)

##### But with

* Rust-native channel ergonomics
* Unified API across topologies
* Clear primitive boundaries

### Misuse prevention (compile-time)

Batch receives return a guard that commits consumption on drop. To keep this sound,
the guard is intentionally *not* an `Iterator<Item = &T>`.

References yielded by `batch.iter()` are tied to the borrow of the batch guard and
cannot outlive it:

````rust,compile_fail
use enso_channel::mpsc;
let (mut tx, mut rx) = mpsc::channel::<u64>(4);
tx.try_send(1).unwrap();

let r: &u64 = {
    let batch = rx.try_recv_at_most(1).unwrap();
    batch.iter().next().unwrap()
};
let _ = *r;
````

And you can’t commit (drop/`finish`) the guard while holding a reference from it:

````rust,compile_fail
use enso_channel::mpsc;
let (mut tx, mut rx) = mpsc::channel::<u64>(4);
tx.try_send(1).unwrap();

let batch = rx.try_recv_at_most(1).unwrap();
let first = batch.iter().next().unwrap();
batch.finish();
let _ = *first;
````

### Public API modules

* [`mpsc`](https://docs.rs/enso-channel/0.1.1/enso_channel/mpsc/index.html): multi-producer, single-consumer
* [`fanout`](https://docs.rs/enso-channel/0.1.1/enso_channel/fanout/index.html): lossless fixed-N fanout (each receiver sees every item)

### Non-goals

 > 
 > To prevent misuse and ambiguity:

* No blocking API
* No dynamic resizing
* No built-in scheduling policy
* No priority queues
* No fairness guarantees beyond lock-free progress
* No **async senders** (see [Async model](#async-model-receiver-only))

**Philosophy:** Scheduling, wake-up strategy, budgeting, priority, and flow control belong
to higher layers.

`enso-channel` intentionally surfaces `Full`, `Empty`, and `Disconnected` instead of hiding
them behind blocking semantics.

The **receiver** side offers optional async support (`recv_async`, `recv_at_most_async`)
behind the `async-receiver` feature flag. The **sender** side remains purely non-blocking
(`try_*` only).

### Async model (receiver only)

`enso-channel` takes an **asymmetric approach to async**: only the receiver side supports
async/await.

This design rests on two observations:

1. **Senders are upstream-driven, receivers are sender-driven.**
   In most systems, senders are paced by external input (IO, timers, other actors).
   They don’t naturally “wait” — they push when data arrives.
   Receivers, by contrast, often have nothing to do until senders produce, making
   async a natural fit for the consumer side.

1. **Cloneable senders make waker coordination expensive.**
   Every sender type in `enso-channel` is `Clone`.
   Adding wakers to the sender side would require synchronising across an arbitrary
   number of cloned handles (mutex, atomic-set, or similar), introducing contention
   at every send. That directly conflicts with the crate’s low-latency, lock-free
   goals — especially when senders rarely need to block in practice.

#### Enabling async

````sh
cargo add enso-channel --features async-receiver
````

````rust,ignore
use enso_channel::mpsc;
use enso_channel::prelude::*;

async fn example(mut rx: mpsc::Receiver<u64>) {
    // Wait for a single item
    let guard = rx.recv_async().await.unwrap();
    println!("{}", *guard);

    // Wait for a batch of up to 8 items
    let batch = rx.recv_at_most_async(8).await.unwrap();
    for item in batch.iter() {
        println!("{}", item);
    }
}
````

#### How it works

* Producers notify receivers each time they commit published data (via
  `commit` / `commit_range`).
* The receiver registers its waker with a lock-free `AtomicWaker`, and the next commit
  wakes it.
* Shutdown (drop of the last sender) also triggers a wake, causing `recv_async` to
  return `None`.

This keeps the sender fast-path entirely free of waker overhead — no lists to scan, no
locks to acquire. The only added cost on the producer side is a single
`AtomicWaker::wake()` call per commit when the `async-receiver` feature is active (zero
cost when disabled).

### Communication Patterns

Unified API across multiple topologies:

|Pattern|Description|
|-------|-----------|
|**MPSC**|Multiple Producer, Single Consumer|
|**Fan-out**|Lossless Broadcast (fixed‑N fanout)|

#### Key Properties

* Receivers may initiate disconnect
* Broadcast topology is fixed at creation
* Disconnection follows RAII semantics
* All patterns support batch operations

### System-Wide Tunability

Tunability in `enso-channel` is not limited to a specific topology.

It emerges from its **batch-native design**.

#### Every pattern supports

|API|Description|
|---|-----------|
|`try_send_at_most` / `try_recv_at_most`|At-most semantics|
|Explicit backpressure|Surface `Full` / `Empty`|

#### This enables systematic control over

* **Contention levels**
* **Cache locality**
* **Burst amortization**
* **Producer/consumer balance**
* **Tail latency behavior**

### Getting Started

#### Installation

````sh
cargo add enso-channel
````

#### Example (MPSC)

````rust
use enso_channel::mpsc;
use enso_channel::prelude::*;
use enso_channel::slot_recycler::ResetWithDefault;

fn main() {
    let (mut tx, mut rx) = mpsc::channel::<u64>(64).unwrap();

    // Single send/recv
    tx.try_send(42).unwrap();
    {
        let guard = rx.try_recv().unwrap();
        assert_eq!(*guard, 42);
    }

    // Batch send
    let mut batch = tx.try_send_at_most(8, ResetWithDefault).unwrap();
    for i in 1..=8 {
        batch.next().unwrap().write(i);
    }
    batch.commit();

    // Batch receive
    let guard = rx.try_recv_at_most(8).unwrap();
    for v in guard.iter() {
        println!("{v}");
    }
}
````

**All topologies share a consistent API shape.**

See [documentation](https://docs.rs/enso-channel) for additional examples.

### Lifecycle / shutdown (RAII)

This crate intentionally does **not** expose an explicit `close()`/`terminate()` API.
Shutdown is expressed through normal Rust endpoint lifecycle:

* dropping the last sender initiates shutdown; receivers may drain already-committed items
  and then observe `Disconnected`;
* dropping the last receiver disconnects senders (subsequent sends return `Disconnected`).

#### Disconnection follows RAII

* Dropping the last sender disconnects receivers (after draining committed items)
* Dropping the last receiver disconnects senders
* Batch guards commit automatically on drop

#### Important caveat

 > 
 > **Disconnection is eventual, not transactional.**

A sender may still successfully publish if a receiver is dropped concurrently.
Already-committed items may never be observed by the application.

For example:

1. A sender reserves a permit / batch permit, and the receiver(s) disconnect afterwards.
1. A sender sends an item successfully, but the receiver(s) disconnect without
   consumption.

#### Panic safety: poison shutdown

The two-phase commit pattern (claim → write → commit) depends on
[`SlotRecycler`](https://docs.rs/enso-channel/0.1.1/enso_channel/slot_recycler/trait.SlotRecycler.html) as the fallback for unwritten slots.
**Your `SlotRecycler` must never panic** — it is the last defense for repairing
stale claims.

If a `SlotRecycler` does panic, the channel contract is broken (claimed slots can
no longer be repaired). The crate responds by **shutting down the entire channel**
to prevent undefined behaviour.

Only **step 1 (claim)** checks for shutdown:

|Step|Shutdown checked?|Rationale|
|----|:---------------:|---------|
|1. Claim slots|yes|natural gate; `Disconnected` returned here|
|2. Write data|no|per-slot checks would hurt the hot path|
|3. Commit|no|data already written; late check can’t help|

In practice, this means a sender that committed data *after* another sender
panicked (but before its own next claim) will see that data silently discarded.
Published items committed *before* the panic remain drainable by receivers, up to
the shutdown boundary.

#### Graceful shutdown without loss

````text
1. Stop producing
2. Drop all senders
3. Drain on the receiver until `Disconnected`
````
<!-- cargo-sync-rdme ]] -->

---

## Performance Characteristics

### Optimized for

- Burst latency
- Tail behavior (p99 / p99.9)
- Busy-spin workloads
- Deterministic bounded pipelines

### Not optimized for

- Blocking workloads
- Async senders (receivers have opt-in async support)
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
- Outputs are printed to stdout and also written as CSV + table to a local output dir (default: `benches/results`, generated and gitignored). Override with `ENSO_SPSC_OUTPUT_DIR` / `ENSO_MPSC_OUTPUT_DIR` / `ENSO_BROADCAST_OUTPUT_DIR`

### To reproduce

```sh
cargo bench --bench spsc
cargo bench --bench mpsc
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

### Fan-out / SPMC scalability (ns/burst)

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

### Contracts (RAII guards)

`enso-channel` is a **lock-free primitive** optimized for fast-path performance.
It intentionally does **not** impose a heavyweight orchestration layer (parking/blocking/spinning,
panic recovery, strict state reconciliation) on every operation.

As a result, some behaviors are **caller contracts**:

- **Send batch commit on drop.** `try_send_at_most*` use the slot recycler to fill
any unwritten slots when a send permit batch is finished or dropped.
Afterwards the written slots will be published and observed by receiver(s).
- **Receive guards commit on drop.** Dropping a receive batch commits the full claimed range.
If you drop it without iterating, unread items are skipped (considered consumed).

If you need stronger "panic containment" or recovery behavior, build it *on top* of this primitive
in your orchestration layer.

> **Lock-free correctness and memory ordering are treated as first-class concerns.**

---

## Roadmap

- [ ] Loom verification
- [ ] Additional workload benchmarks
- [ ] More usage examples (async receiver)
- [ ] Documentation expansion

---

## License

Licensed under either:

- Apache License, Version 2.0
- MIT License
