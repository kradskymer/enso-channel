#![allow(dead_code)]

pub mod concurrency;
pub mod contracts;
pub mod exclusive;
pub mod fanout;
pub mod queue;
pub mod shared;

/// Macro to generate contract tests for a channel implementation.
///
/// Usage:
/// ```ignore
/// generate_contract_tests!(MyChannel, [
///     contract_fifo_order,
///     contract_capacity_backpressure,
///     contract_recv_empty,
/// ]);
/// ```
#[macro_export]
macro_rules! generate_contract_tests {
    ($impl_type:ty, [$($test_fn:ident),* $(,)?]) => {
        $(
            paste::paste! {
                #[test]
                fn [< $test_fn >]() {
                    harness::contracts::$test_fn::<$impl_type>();
                }
            }
        )*
    };
}

/// Macro to generate contract tests for a channel implementation with a custom prefix.
///
/// Usage:
/// ```ignore
/// generate_contract_tests_prefixed!(spsc, Spsc, [
///     contract_fifo_order,
///     contract_capacity_backpressure,
/// ]);
/// ```
#[macro_export]
macro_rules! generate_contract_tests_prefixed {
    ($prefix:ident, $impl_type:ty, [$($test_fn:ident),* $(,)?]) => {
        $(
            paste::paste! {
                #[test]
                fn [< $prefix _ $test_fn >]() {
                    harness::contracts::$test_fn::<$impl_type>();
                }
            }
        )*
    };
}
