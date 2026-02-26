//! Work distribution channels ("work queue").
//!
//! This topology is intended for a worker-pool style workload where **each item is
//! processed by exactly one consumer** and multiple receivers may compete to claim work.
//!
//! Construction follows the crossbeam-style pattern: `let (tx, rx) = channel(..)` and
//! then `rx` (and for MPMC also `tx`) can be cloned to create additional endpoints.
//!
//! Note: despite the historical name `worksteal`, this module is not necessarily a
//! classic deque-based "work stealing" scheduler.

pub mod mpmc;
pub mod spmc;
