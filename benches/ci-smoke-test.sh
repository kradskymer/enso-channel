#!/bin/bash
# CI smoke test for benchmarks
# This script runs quick benchmark smoke tests to verify compilation and basic functionality
# without the overhead of full performance measurements.

set -e

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$BENCH_DIR")"

cd "$PROJECT_ROOT"

echo "Building benchmarks..."
cargo bench --no-run

echo ""
echo "Running SPSC benchmark (smoke test)..."
ENSO_SPSC_WARMUP_BURSTS=100 \
ENSO_SPSC_MEASURE_BURSTS=500 \
ENSO_SPSC_BURST_SIZES=1,16 \
ENSO_SPSC_TIMEOUT_SECS=60 \
ENSO_CHANNEL_PINNING=off \
cargo bench --bench spsc -- --profile-time 1

echo ""
echo "Running MPSC benchmark (smoke test)..."
ENSO_MPSC_WARMUP_BURSTS=100 \
ENSO_MPSC_MEASURE_BURSTS=500 \
ENSO_MPSC_BURST_SIZES=1,16 \
ENSO_MPSC_TIMEOUT_SECS=60 \
ENSO_CHANNEL_PINNING=off \
cargo bench --bench mpsc -- --profile-time 1

echo ""
echo "Running MPMC benchmark (smoke test)..."
ENSO_MPMC_WARMUP_BURSTS=100 \
ENSO_MPMC_MEASURE_BURSTS=500 \
ENSO_MPMC_BURST_SIZES=1,16 \
ENSO_MPMC_TIMEOUT_SECS=60 \
ENSO_CHANNEL_PINNING=off \
cargo bench --bench mpmc -- --profile-time 1

echo ""
echo "Running Broadcast benchmark (smoke test)..."
ENSO_BROADCAST_WARMUP_BURSTS=100 \
ENSO_BROADCAST_MEASURE_BURSTS=500 \
ENSO_BROADCAST_BURST_SIZES=1,16 \
ENSO_BROADCAST_TIMEOUT_SECS=60 \
ENSO_CHANNEL_PINNING=off \
cargo bench --bench broadcast -- --profile-time 1

echo ""
echo "✓ All benchmark smoke tests passed!"
