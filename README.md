# enso_channel

[![Crates.io](https://img.shields.io/crates/v/enso_channel.svg)](https://crates.io/crates/enso_channel)
[![Documentation](https://docs.rs/enso_channel/badge.svg)](https://docs.rs/enso_channel)
[![License](https://img.shields.io/crates/l/enso_channel.svg)](LICENSE-MIT)

High-performance lock-free ring-buffer channels with native batch support for several communication patterns.

Inspired by [LMAX Disruptor](https://github.com/LMAX-Exchange/disruptor) and [crossbeam-channel](https://github.com/crossbeam-rs/crossbeam).

## Getting Started

Add to your project:

```sh
cargo add enso_channel
```

Basic usage (SPSC):

```rust
fn main() {
    use enso_channel::exclusive::spsc;

    let (mut tx, mut rx) = spsc::channel::<u64>(64);

    // Single send/recv
    tx.try_send(42).unwrap();
    {
        let guard = rx.try_recv().unwrap();
        assert_eq!(*guard, 42);
        // Guard should release on drop, allowing the next item to be observed.
    }

    // Batch send
    let mut batch = tx.try_send_many_default(8).unwrap();
    for i in 1..=8 {
        batch.write_next(i);
    }
    batch.finish();

    // Batch send with at most semantics.
    let mut batch = tx.try_send_at_most(8, || 0).unwrap();
    batch.fill_with(|| 100);
    drop(batch);

    // Batch recv
    let iter = rx.try_recv_many(8).unwrap();
    let mut count = 0;
    for val in iter {
        count += 1;
        println!("{val}");
    }
    assert_eq!(count, 8);

    // Batch recv with at most semantics.
    let iter = rx.try_recv_at_most(20).unwrap();
    let mut count = 0;
    for val in iter {
        count += 1;
        println!("{val}");
    }
    assert_eq!(count, 8);
}
```

## Features

- Low latency and high throughput.
- Batch support for sending and receiving for all patterns.
- Multiple communication patterns:
  - SPSC (Single Producer Single Consumer)
  - MPSC (Multi Producer Single Consumer)
  - Fanout (Single/Multiple Producer Multiple Consumer, lossless)
  - Work Queue (Multiple Producer Multiple Consumer Work Distribution)

## Core Design Principles

- Lock-free
- Bounded capacity
- Pre-allocated memory
- Native batch support for both sending and receiving
- Channels style API
- Better control and tunability with batching parameters (see [work queue bench overview](#work-queue))

## Trade-offs

- No dynamic resizing of the channel capacity.
- Static topology for fan-out pattern (number of consumers are fixed).
- No blocking API
- Back pressure is surfaced via `try_*` errors; retry/backoff is caller-controlled.

## Disconnection (RAII)

`enso_channel` intentionally does **not** expose an explicit `close()`/`terminate()` API.
Instead, disconnection is expressed through RAII similar to std mpsc channels:

- Dropping the last sender disconnects receivers after already-committed items are drained.
- Dropping the last receiver disconnects senders.
- Send/recv batch guards commit automatically on drop; forgetting to drop a guard will
  delay visibility/progress.

### Concurrent disconnection caveat

In concurrent code, receiver-initiated disconnection is best-effort:
a sender operation may still publish successfully even if the receiver is dropped concurrently,
and published items may never be observed by the application.

Example:

```rust
use enso_channel::exclusive::mpsc;

fn main() {
    let (mut sender, receiver) = mpsc::channel::<u64>(16);
    let mut batch = sender.try_send_many_default(8).unwrap();
    // Disconnect receiver while batch guard is still alive.
    drop(receiver);
    // Batch guard should still be able to publish successfully, but items will never be observed.
    batch.fill_with(|| 0);
    batch.finish();
    // This time will return `Disconnected`
    let batch = sender.try_send_many_default(8);
    matches!(batch, Err(enso_channel::errors::TrySendError::Disconnected));
}
```

### Graceful shutdown

If you **do not want to lose any items**, initiate shutdown from the **producer** side:
stop producing new items, drop senders, then drain on the consumer until it returns `Disconnected`.

Practical pattern for a non-blocking drain loop (producer-first shutdown):

```rust
use enso_channel::errors::TryRecvAtMostError;

fn main() {
    let (mut sender, mut receiver) = enso_channel::exclusive::mpsc::channel::<u64>(16);
    sender.try_send(42).unwrap();
    // Drop sender to initiate shutdown.
    drop(sender);
    // Receiver can still drain already-committed items.
    assert_eq!(*receiver.try_recv().unwrap(), 42);
    // After draining, receiver observes `Disconnected`.
    loop {
        match receiver.try_recv_at_most(64) {
            Ok(iter) => {
                for v in iter {
                    let _ = *v;
                }
            }
            Err(TryRecvAtMostError::Empty) => {
                // retry/backoff/spin: caller-controlled
            }
            Err(TryRecvAtMostError::Disconnected) => break,
        }
    }
}
```

## Benchmark Results (Selected)

All the following benchmarks are run on a MacBook M4-Pro with 10 cores and 24GB memory.
The results are generated from [latency.rs](./benches/latency.rs) and detailed results can be found in [bench_results.txt](./benches/bench_results.txt).

**NOTE:**

- All the latency numbers are in nanoseconds (ns) and lower is better.
- `p_batch` and `c_batch` refer to the producer and consumer batch sizes respectively.
- SPSC, MPSC and Fanout are measured with RTT
- Work Queue is measured with one-way latency (producer to consumer)

### Running benchmarks

To run Criterion benchmarks and supply options to Criterion, place the Criterion flags after `--` so cargo forwards them to the benchmark binary.

Examples:

- `cargo bench --bench spsc -- --save-baseline spsc`

> The `--save-baseline` option **requires** a baseline name (it is not a boolean flag).

To inspect available Criterion options for a built bench binary run:

- `cargo bench --bench spsc -- --help`

### SPSC

| Scenario                                       | Mean  | p50 | p99 | p99.9 |
| ---------------------------------------------- | ----- | --- | --- | ----- |
| enso_channel (single)                          | 142.0 | 125 | 250 | 458   |
| enso_channel (p_batch=8 c_batch=8 full RTT)    | 195.7 | 167 | 375 | 584   |
| enso_channel (p_batch=64 c_batch=64 full RTT)  | 378.9 | 375 | 708 | 1083  |
| enso_channel (p_batch=8 c_batch=8 amortized)   | 24.5  | 20  | 46  | 73    |
| enso_channel (p_batch=64 c_batch=64 amortized) | 5.9   | 5   | 11  | 16    |
| crossbeam                                      | 192.2 | 208 | 292 | 417   |
| flume                                          | 305.5 | 291 | 666 | 4792  |

### MPSC

| Scenario (2 producers)                         | Mean  | p50 | p99  | p99.9 |
| ---------------------------------------------- | ----- | --- | ---- | ----- |
| enso_channel (single)                          | 52.9  | 42  | 209  | 292   |
| enso_channel (p_batch=4 c_batch=4 full RTT)    | 44.4  | 42  | 167  | 291   |
| enso_channel (p_batch=32 c_batch=32 full RTT)  | 45.3  | 42  | 84   | 167   |
| enso_channel (p_batch=4 c_batch=4 amortized)   | 11.1  | 10  | 41   | 72    |
| enso_channel (p_batch=32 c_batch=32 amortized) | 1.4   | 1   | 2    | 5     |
| crossbeam                                      | 95.1  | 83  | 250  | 292   |
| flume                                          | 221.6 | 41  | 4083 | 8792  |

| Scenario (4 producers)                         | Mean  | p50 | p99   | p99.9 |
| ---------------------------------------------- | ----- | --- | ----- | ----- |
| enso_channel (single)                          | 154.3 | 167 | 292   | 333   |
| enso_channel (p_batch=4 c_batch=4 full RTT)    | 156.4 | 167 | 292   | 417   |
| enso_channel (p_batch=32 c_batch=32 full RTT)  | 125.1 | 125 | 250   | 333   |
| enso_channel (p_batch=4 c_batch=4 amortized)   | 39.1  | 41  | 73    | 104   |
| enso_channel (p_batch=32 c_batch=32 amortized) | 3.9   | 3   | 7     | 10    |
| crossbeam                                      | 104.1 | 84  | 250   | 333   |
| flume                                          | 768.0 | 42  | 11333 | 17750 |

### Fanout

| Scenario (1 producer 2 consumers)              | Mean  | p50 | p99  | p99.9 |
| ---------------------------------------------- | ----- | --- | ---- | ----- |
| enso_channel (single)                          | 458.0 | 375 | 1250 | 3958  |
| enso_channel (p_batch=4 c_batch=4 full RTT)    | 215.6 | 167 | 541  | 708   |
| enso_channel (p_batch=64 c_batch=64 full RTT)  | 225.3 | 209 | 250  | 375   |
| enso_channel (p_batch=4 c_batch=4 amortized)   | 53.9  | 41  | 135  | 177   |
| enso_channel (p_batch=64 c_batch=64 amortized) | 3.5   | 3   | 3    | 5     |

| Scenario (1 producer 4 consumers)              | Mean  | p50 | p99  | p99.9 |
| ---------------------------------------------- | ----- | --- | ---- | ----- |
| enso_channel (single)                          | 792.5 | 750 | 1375 | 7500  |
| enso_channel (p_batch=4 c_batch=4 full RTT)    | 443.6 | 417 | 584  | 4125  |
| enso_channel (p_batch=64 c_batch=64 full RTT)  | 497.9 | 500 | 584  | 875   |
| enso_channel (p_batch=4 c_batch=4 amortized)   | 110.9 | 104 | 146  | 1031  |
| enso_channel (p_batch=64 c_batch=64 amortized) | 7.8   | 7   | 9    | 13    |

### Work Queue

| Scenario (2 producers 2 consumers)             | Mean      | p50     | p99     | p99.9   |
| ---------------------------------------------- | --------- | ------- | ------- | ------- |
| enso_channel (single)                          | 916854.6  | 852791  | 2915125 | 2944791 |
| enso_channel (p_batch=4 c_batch=4 full RTT)    | 209066.9  | 375     | 704125  | 705458  |
| enso_channel (p_batch=64 c_batch=64 full RTT)  | 180003.8  | 143750  | 271833  | 276500  |
| enso_channel (p_batch=4 c_batch=4 amortized)   | 52266.7   | 93      | 176031  | 176364  |
| enso_channel (p_batch=64 c_batch=64 amortized) | 2812.6    | 2246    | 4247    | 4320    |
| crossbeam                                      | 1724578.0 | 1475208 | 2951833 | 2982792 |
| enso_channel (p_batch=4 c_batch=16 full RTT)   | 578.4     | 458     | 3000    | 22417   |
| enso_channel (p_batch=8 c_batch=32 full RTT)   | 342.1     | 333     | 791     | 3458    |
| enso_channel (p_batch=4 c_batch=16 amortized)  | 36.2      | 28      | 187     | 1401    |
| enso_channel (p_batch=8 c_batch=32 amortized)  | 10.7      | 10      | 24      | 108     |

| Scenario (2 producers 4 consumers)            | Mean     | p50    | p99    | p99.9  |
| --------------------------------------------- | -------- | ------ | ------ | ------ |
| enso_channel (p_batch=4 c_batch=16 full RTT)  | 68429.7  | 625    | 265792 | 267542 |
| enso_channel (p_batch=4 c_batch=32 full RTT)  | 840.6    | 833    | 1625   | 9917   |
| enso_channel (p_batch=8 c_batch=64 full RTT)  | 497232.6 | 532625 | 553416 | 557459 |
| enso_channel (p_batch=4 c_batch=16 amortized) | 4276.9   | 39     | 16612  | 16721  |
| enso_channel (p_batch=4 c_batch=32 amortized) | 26.3     | 26     | 50     | 309    |
| enso_channel (p_batch=8 c_batch=64 amortized) | 7769.3   | 8322   | 8647   | 8710   |

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
