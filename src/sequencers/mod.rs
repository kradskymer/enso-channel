//! Internal sequencing building blocks.
//!
//! This module contains the *sealed* traits and concrete implementations that power the
//! channel implementations (single-consumer, fixed-N broadcast, work-queue).
//!
//! ## How the pieces fit
//!
//! Conceptually there are two directions of coordination:
//!
//! - **Producer → Consumer**: producers publish sequences and consumers discover what is
//!   available to read.
//!   - Concrete mechanism depends on topology:
//!     - single-producer: a `published` cursor
//!     - multi-producer: per-slot `slot_states`
//!   - The consumer-facing interface is [`PublisherSeqGate`].
//!
//! - **Consumer → Producer**: consumers advance (consume/free) progress and producers discover
//!   what capacity can be safely reused.
//!   - Concrete mechanism depends on topology:
//!     - single-consumer / fixed-N broadcast: per-consumer `consumed` cursor(s)
//!     - work-queue: per-slot consumed states + a claimed cursor sentinel for disconnect
//!   - The producer-facing interface is [`ConsumerSeqGate`].
//!
//! [`Sequencer`] implementations sit on top of these gates:
//!
//! - Producer-side sequencers use `ConsumerSeqGate::max_consumed` to compute how far they can
//!   claim without overwriting unconsumed data.
//! - Consumer-side sequencers use `PublisherSeqGate::max_published` to compute how far they can
//!   claim without reading unpublished data.
//!
//! ## Memory ordering model (why the fences exist)
//!
//! Many hot paths use a "cursor/state observed, then access ring memory" pattern.
//! The intent is:
//!
//! - If a consumer observes that a sequence is **published**, subsequent reads of that slot's
//!   payload must observe the producer's writes.
//! - If a producer observes that a slot is **consumed/freed**, subsequent writes reusing that
//!   slot must not be reordered before the observation.
//!
//! Implementations may use one of two equivalent observation styles:
//!
//! 1) **Direct acquire load**
//!
//!    - Writer: `state.store(v, Release)` (or `fence(Release); state.store(v, Relaxed)`)
//!    - Reader: `state.load(Acquire)`
//!
//! 2) **Relaxed load + conditional acquire fence**
//!
//!    - Writer: `state.store(v, Release)` (or `fence(Release); state.store(v, Relaxed)`)
//!    - Reader: `let v = state.load(Relaxed); if v crosses the queried boundary { fence(Acquire) }`
//!
//! Style (2) is used to keep the common "not yet available" path cheap while still ensuring
//! that, once the observed value is sufficient for the caller to proceed, subsequent memory
//! operations (payload reads or slot reuse writes) are not reordered before the observation.
//!
//! Note that this module also defines *disconnect* semantics via sentinels and/or counters.
//! Those are orthogonal to the publish/consume visibility rules and are surfaced via
//! `is_shutdown_open()` checks on returned [`Sequence`] values and the dedicated
//! [`ConsumerSeqGate::is_disconnected`] fast-path.

mod con_ex;
mod con_fanout;
mod con_queue;
mod pub_mul;

pub(crate) mod sealed {
    pub trait Sealed {}
}

pub(crate) use con_ex::{ExclusiveConSeqGate, ExclusiveConsumerSequencer};
pub(crate) use con_fanout::{FanoutConSeqGate, FanoutConsumerSequencer};
pub(crate) use con_queue::{QueuedConSeqGate, QueuedConsumerSequencer, QueuedConsumerWiring};
pub(crate) use pub_mul::{MultiPubSeqGate, MultiPublisherSequencer};

use crate::Sequence;
use crate::errors::TryClaimError;

/// Internal sequencing contract used by publisher/consumer infrastructure.
///
/// This trait is intentionally **sealed** and `#[doc(hidden)]`.
///
/// Users of the crate should not need to import or interact with it.
#[doc(hidden)]
pub trait Sequencer: sealed::Sealed {
    /// Attempts to reserve the next `n` sequences for publishing.
    /// If successful, returns (next_seq, highest_seq) where highest_seq == next_seq + n - 1.
    /// If insufficient capacity, returns `TryClaimError::Insufficient` with the number of missing sequences.
    fn try_claim_n(&mut self, n: i64) -> Result<(Sequence, Sequence), TryClaimError>;

    /// Attempts to reserve the next sequence for publishing.
    fn try_claim(&mut self) -> Result<Sequence, TryClaimError>;

    /// Attempts to reserve up to `limit` sequences, claiming as many as available (1..=limit).
    /// Returns a tuple of (start_seq, end_seq) where count is the actual number of sequences claimed.
    /// If limit > buffer size, it will only attempt to claim up to buffer size.
    /// If no sequences are available, returns `TryClaimError::Empty`.
    fn try_claim_at_most(&mut self, limit: i64) -> Result<(Sequence, Sequence), TryClaimError>;

    /// Marks the sequence `seq` as committed.
    fn commit(&self, seq: Sequence);

    /// Marks the range of sequences from `start_seq` to `end_seq` (inclusive) as committed.
    fn commit_range(&self, start_seq: Sequence, end_seq: Sequence);
}

/// Gate interface for consumer to coordinate with publisher sequences.
pub(crate) trait PublisherSeqGate {
    /// Returns the highest published sequence in the range [`next_seq`, `end_seq`].
    /// If no sequences are available in the range, then `next_seq - 1` will be returned.
    fn max_published(&self, next_seq: Sequence, end_seq: Sequence) -> Sequence;

    /// Returns publisher shutdown boundary sequence.
    ///
    /// Semantics:
    /// - `Sequence::SHUTDOWN_OPEN` => publisher side is still open.
    /// - any other value => publisher side is disconnected and the value is
    ///   the last sequence boundary that consumers may drain.
    fn shutdown_sequence(&self) -> Sequence;
}

/// Gate interface for publisher to coordinate with consumer sequences.
pub(crate) trait ConsumerSeqGate {
    /// Returns the minimum consumed sequence across all consumers.
    ///
    /// For fixed-N broadcast semantics, this is the slowest consumer (minimum of all consumer positions).
    /// The `next_seq` and `end_seq` parameters provide context about the publisher's query range,
    /// but the returned value is the actual consumer position, which may be less than `next_seq`.
    ///
    /// Returns minimum consumed sequence across all consumers.
    ///
    /// Semantics:
    /// - `Sequence::SHUTDOWN_OPEN` => consumer side is disconnected.
    /// - any other value => valid max-consumed cursor.
    fn max_consumed(&self, next_seq: Sequence, end_seq: Sequence) -> Sequence;

    /// Returns true if the consumer side is disconnected.
    ///
    /// This is intended to be a constant-time check used by publisher fast-paths
    /// to avoid claiming/publishing after all consumers are dropped.
    fn is_disconnected(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    };

    use super::{
        ConsumerSeqGate, ExclusiveConsumerSequencer, FanoutConsumerSequencer,
        MultiPublisherSequencer, PublisherSeqGate, QueuedConsumerWiring, Sequencer,
    };
    use crate::{Cursor, RingBufferMeta, Sequence, errors::TryClaimError};

    #[derive(Clone)]
    struct CursorConsumerGate {
        consumed: Arc<AtomicI64>,
    }

    impl ConsumerSeqGate for CursorConsumerGate {
        fn max_consumed(&self, _next_seq: Sequence, _end_seq: Sequence) -> Sequence {
            self.consumed.load(Ordering::Acquire).into()
        }

        fn is_disconnected(&self) -> bool {
            self.consumed.load(Ordering::Acquire) == Sequence::SHUTDOWN_OPEN.value()
        }
    }

    #[derive(Clone)]
    struct CursorPublisherGate {
        published: Arc<AtomicI64>,
        shutdown_sequence: Arc<AtomicI64>,
    }

    impl PublisherSeqGate for CursorPublisherGate {
        fn max_published(&self, _next_seq: Sequence, _end_seq: Sequence) -> Sequence {
            Sequence::new(self.published.load(Ordering::Acquire))
        }

        fn shutdown_sequence(&self) -> Sequence {
            self.shutdown_sequence.load(Ordering::Acquire).into()
        }
    }

    fn run_publisher_test_suite<S, F>(make: F)
    where
        S: Sequencer,
        F: Fn(Arc<CursorConsumerGate>, RingBufferMeta) -> S,
    {
        let ring = RingBufferMeta::new(8);
        let consumed = Arc::new(AtomicI64::new(Sequence::INIT.value()));
        let gate = Arc::new(CursorConsumerGate {
            consumed: consumed.clone(),
        });

        let mut seq = make(gate.clone(), ring);

        // 1. Initial Capacity
        // Should be able to claim 8 slots (0..7)
        let (_, end_seq) = seq.try_claim_n(8).expect("Should claim full capacity");
        assert_eq!(end_seq, Sequence::new(7));

        // 2. Backpressure
        // Next claim should fail
        match seq.try_claim() {
            Err(TryClaimError::Insufficient { missing }) => assert_eq!(missing, 1),
            Ok(_) => panic!("Should be backpressured"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }

        // 3. Advance Consumer
        // Consumer processes 1 item (seq 0)
        consumed.store(0, Ordering::Release);

        // Should be able to claim 1 more (seq 8)
        let claimed = seq
            .try_claim()
            .expect("Should claim after consumer advance");
        assert_eq!(claimed, Sequence::new(8));

        // 4. Disconnect
        consumed.store(Sequence::SHUTDOWN_OPEN.value(), Ordering::Release);
        match seq.try_claim() {
            Err(TryClaimError::Shutdown) => {}
            Ok(_) => panic!("Should be disconnected"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    fn run_consumer_test_suite<S, F>(make: F)
    where
        S: Sequencer,
        F: Fn(Arc<CursorPublisherGate>, RingBufferMeta) -> S,
    {
        let published = Arc::new(AtomicI64::new(Sequence::INIT.value())); // -1
        let shutdown_sequence = Arc::new(AtomicI64::new(Sequence::SHUTDOWN_OPEN.value()));
        let gate = Arc::new(CursorPublisherGate {
            published: published.clone(),
            shutdown_sequence: shutdown_sequence.clone(),
        });
        let ring_meta = RingBufferMeta::new(8);

        let mut seq = make(gate.clone(), ring_meta);

        // 1. Empty
        match seq.try_claim() {
            Err(TryClaimError::Insufficient { .. }) => {}
            Ok(_) => panic!("Should be empty"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }

        // 2. Publish
        published.store(0, Ordering::Release);
        let claimed = seq.try_claim().expect("Should claim published item");
        assert_eq!(claimed, Sequence::new(0));
        seq.commit(claimed);

        // 3. Shutdown
        // Publish up to 1, but shutdown at 1
        published.store(1, Ordering::Release);
        shutdown_sequence.store(1, Ordering::Release);

        // Should claim 1
        let claimed = seq.try_claim().expect("Should claim item before shutdown");
        assert_eq!(claimed, Sequence::new(1));
        seq.commit(claimed);

        // Next should be disconnected
        match seq.try_claim() {
            Err(TryClaimError::Shutdown) => {}
            Ok(_) => panic!("Should be disconnected"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn multi_publisher_sequencer_generic() {
        run_publisher_test_suite(MultiPublisherSequencer::new);
    }

    #[test]
    fn exclusive_consumer_sequencer_generic() {
        run_consumer_test_suite(|gate, ring_meta| {
            let consumed = Arc::new(Cursor::new(Sequence::INIT));
            ExclusiveConsumerSequencer::new(consumed, gate, ring_meta)
        });
    }

    #[test]
    fn fanout_consumer_sequencer_generic() {
        run_consumer_test_suite(|gate, ring_meta| {
            let consumed = Arc::new(Cursor::new(Sequence::INIT));
            let connected = Arc::new(std::sync::atomic::AtomicUsize::new(1));
            FanoutConsumerSequencer::new(gate, consumed, connected, ring_meta)
        });
    }

    #[test]
    fn queued_consumer_sequencer_generic() {
        run_consumer_test_suite(|gate, ring_meta| {
            let ring = RingBufferMeta::new(8);
            let (_, wiring) = QueuedConsumerWiring::new(ring);
            wiring.build_consumer(gate, ring_meta)
        });
    }

    #[test]
    fn queue_consumers_split_work_across_clones() {
        let ring_meta = RingBufferMeta::new(8);
        let published = Arc::new(AtomicI64::new(3));
        let shutdown_sequence = Arc::new(AtomicI64::new(Sequence::SHUTDOWN_OPEN.value()));
        let producer_gate = Arc::new(CursorPublisherGate {
            published,
            shutdown_sequence,
        });

        let (_gate, wiring) = QueuedConsumerWiring::new(ring_meta);
        let mut rx1 = wiring.build_consumer(producer_gate.clone(), ring_meta);
        let mut rx2 = rx1.clone();

        let s1 = rx1.try_claim().expect("rx1 claim");
        let s2 = rx2.try_claim().expect("rx2 claim");
        assert_ne!(s1, s2);
        assert_eq!(
            (s1.value().min(s2.value()), s1.value().max(s2.value())),
            (0, 1)
        );

        rx1.commit(s1);
        rx2.commit(s2);
    }

    fn run_publisher_at_most_test_suite<S, F>(make: F)
    where
        S: Sequencer,
        F: Fn(Arc<CursorConsumerGate>, RingBufferMeta) -> S,
    {
        let ring = RingBufferMeta::new(8);
        let consumed = Arc::new(AtomicI64::new(Sequence::INIT.value()));
        let gate = Arc::new(CursorConsumerGate {
            consumed: consumed.clone(),
        });

        let mut seq = make(gate.clone(), ring);

        // 1. Full capacity available - claim all requested
        let (start_seq, end_seq) = seq.try_claim_at_most(5).expect("Should claim 5");
        assert_eq!(start_seq, Sequence::new(0));
        assert_eq!(end_seq, Sequence::new(4));
        seq.commit_range(start_seq, end_seq);

        // 2. Claim rest of capacity
        let (start_seq, end_seq) = seq.try_claim_at_most(10).expect("Should claim remaining 3");
        assert_eq!(end_seq.value() - start_seq.value() + 1, 3); // Only 3 slots remain (ring size 8)
        assert_eq!(end_seq, Sequence::new(7));
        seq.commit_range(start_seq, end_seq);

        // 3. Zero capacity - return Empty error (all 8 slots taken, nothing consumed yet)
        let result = seq.try_claim_at_most(1);
        match result {
            Err(TryClaimError::Empty) => {}
            Ok(_) => panic!("Should be full"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }

        // 4. Free some slots, then claim partial
        consumed.store(5, Ordering::Release); // Free slots 0-5
        let (start_seq, end_seq) = seq.try_claim_at_most(10).expect("Should claim partial");
        let count = end_seq.value() - start_seq.value() + 1;
        assert_eq!(count, 6); // Can claim 6 (ring is 8, 2 occupied at 6-7, 6 freed at 0-5)
        seq.commit_range(start_seq, end_seq);

        // 5. Disconnect
        consumed.store(Sequence::SHUTDOWN_OPEN.value(), Ordering::Release);
        match seq.try_claim_at_most(1) {
            Err(TryClaimError::Shutdown) => {}
            Ok(_) => panic!("Should be disconnected"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    fn run_consumer_at_most_test_suite<S, F>(make: F)
    where
        S: Sequencer,
        F: Fn(Arc<CursorPublisherGate>) -> S,
    {
        let published = Arc::new(AtomicI64::new(Sequence::INIT.value()));
        let shutdown_sequence = Arc::new(AtomicI64::new(Sequence::SHUTDOWN_OPEN.value()));
        let gate = Arc::new(CursorPublisherGate {
            published: published.clone(),
            shutdown_sequence: shutdown_sequence.clone(),
        });

        let mut seq = make(gate.clone());

        // 1. Empty - return Empty error
        match seq.try_claim_at_most(1) {
            Err(TryClaimError::Empty) => {}
            Ok(_) => panic!("Should be empty"),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }

        // 2. Publish some items - claim all requested
        published.store(4, Ordering::Release);
        let (start_seq, end_seq) = seq.try_claim_at_most(5).expect("Should claim available");
        assert_eq!(end_seq.value() - start_seq.value() + 1, 5);
        assert_eq!(end_seq, Sequence::new(4));
        seq.commit_range(start_seq, end_seq);

        // 3. Partial items - claim what's available
        published.store(6, Ordering::Release);
        let (start_seq, end_seq) = seq.try_claim_at_most(10).expect("Should claim partial");
        assert_eq!(end_seq.value() - start_seq.value() + 1, 2);
        assert_eq!(end_seq, Sequence::new(6));
        seq.commit_range(start_seq, end_seq);

        // 4. Shutdown
        shutdown_sequence.store(6, Ordering::Release);
        match seq.try_claim_at_most(1) {
            Err(TryClaimError::Shutdown) => {}
            Ok(v) => panic!("Should be disconnected, {:?}", v),
            Err(e) => panic!("Unexpected error: {:?}", e),
        }
    }

    #[test]
    fn multi_publisher_sequencer_at_most() {
        run_publisher_at_most_test_suite(MultiPublisherSequencer::new);
    }

    #[test]
    fn exclusive_consumer_sequencer_at_most() {
        run_consumer_at_most_test_suite(|gate| {
            let consumed = Arc::new(Cursor::new(Sequence::INIT));
            let ring_meta = RingBufferMeta::new(8);
            ExclusiveConsumerSequencer::new(consumed, gate, ring_meta)
        });
    }

    #[test]
    fn fanout_consumer_sequencer_at_most() {
        run_consumer_at_most_test_suite(|gate| {
            let consumed = Arc::new(Cursor::new(Sequence::INIT));
            let connected = Arc::new(std::sync::atomic::AtomicUsize::new(1));
            let ring_meta = RingBufferMeta::new(8);
            FanoutConsumerSequencer::new(gate, consumed, connected, ring_meta)
        });
    }

    #[test]
    fn queued_consumer_sequencer_at_most() {
        run_consumer_at_most_test_suite(|gate| {
            let ring_meta = RingBufferMeta::new(8);
            let (_, wiring) = QueuedConsumerWiring::new(ring_meta);
            wiring.build_consumer(gate, ring_meta)
        });
    }
}
