//! Internal sequencing building blocks.
//!
//! This module contains the *sealed* traits and concrete implementations that power the
//! channel implementations (single-consumer, fixed-N broadcast).
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
//!   - The consumer-facing interface is [`ProducerBarrier`].
//!
//! - **Consumer → Producer**: consumers advance (consume/free) progress and producers discover
//!   what capacity can be safely reused.
//!   - Concrete mechanism depends on topology:
//!     - single-consumer / fixed-N broadcast: per-consumer `consumed` cursor(s)
//!     - work-queue: per-slot consumed states + a claimed cursor sentinel for disconnect
//!   - The producer-facing interface is [`ConsumerBarrier`].
//!
//! [`Sequencer`] implementations sit on top of these gates:
//!
//! - Producer-side sequencers use `ConsumerSeqGate::max_consumed` to compute how far they can
//!   claim without overwriting unconsumed data.
//! - Consumer-side sequencers use `PublisherSeqGate::max_published` to compute how far they can
//!   claim without reading unpublished data.

mod con_ex;
mod con_fanout;
mod pub_mul;

pub(crate) use con_ex::{ExclusiveConSeqGate, ExclusiveConsumerSequencer};
pub(crate) use con_fanout::{FanoutConSeqGate, FanoutConsumerSequencer};
pub(crate) use pub_mul::{MultiPubSeqGate, MultiPublisherSequencer};

use crate::errors::TryClaimError;
use crate::Sequence;

/// Internal sequencing contract used by publisher/consumer infrastructure.
///
/// This trait is intentionally **sealed** and `#[doc(hidden)]`.
///
/// Users of the crate should not need to import or interact with it.
#[doc(hidden)]
pub(crate) trait Sequencer {
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

    /// Immediately terminates the sequencer, signalling shutdown to all peers.
    ///
    /// This is used when a sender panics during slot recycling (e.g. in the
    /// [`SlotRecycler`](crate::SlotRecycler) callback). After `terminate()` returns:
    /// - all future `try_claim` / `try_claim_at_most` calls will return
    ///   [`TryClaimError::Shutdown`],
    /// - consumers will observe [`TryRecvError::Disconnected`](crate::errors::TryRecvError::Disconnected).
    fn terminate(&self) {
        unimplemented!("By default consumer should not use this explicitly")
    }
}

/// A barrier that prevents consumers from reading unpublished.
pub(crate) trait ProducerBarrier {
    /// Returns the max sequence that a consumer can read to
    /// This will find the max available sequence between [next_seq, end_seq] inclusive.
    /// If no sequence is available in the range, next_seq - 1 will return
    ///
    /// If the sequencer is shutdown, [`SlotState::is_shutdown`] will be true.
    fn max_published(&self, next_seq: Sequence, end_seq: Sequence) -> SlotState;
}

/// Gate interface for publisher to coordinate with consumer sequences.
pub(crate) trait ConsumerBarrier {
    /// Returns the minimum consumed sequence across all consumers together
    /// with a shutdown flag.
    ///
    /// For fixed-N broadcast semantics, this is the slowest consumer (minimum
    /// of all consumer positions).  `SlotState::is_shutdown()` is true when
    /// the consumer side has disconnected.
    fn max_consumed(&self) -> SlotState;
}

/// Combines a sequence value with a shutdown flag.
///
/// Returned by [`SlotStateGroup::scan_available_until`] so callers receive both
/// the last contiguous available sequence and a shutdown signal in one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SlotState {
    seq: Sequence,
    shutdown: bool,
}

impl SlotState {
    pub(crate) fn new(seq: Sequence, shutdown: bool) -> Self {
        Self { seq, shutdown }
    }

    #[inline]
    pub(crate) fn sequence(self) -> Sequence {
        self.seq
    }

    #[inline]
    pub(crate) fn is_shutdown(self) -> bool {
        self.shutdown
    }
}
