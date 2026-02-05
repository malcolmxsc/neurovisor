#!/bin/bash
# Build eBPF programs for NeuroVisor
#
# This script compiles the eBPF programs to the bpfel (little-endian) target
# and copies them to target/ebpf/ for inclusion in the main binary.
#
# Requirements:
#   - Rust nightly toolchain
#   - bpf-linker (cargo install bpf-linker)

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EBPF_DIR="$SCRIPT_DIR/ebpf-programs"
OUTPUT_DIR="$SCRIPT_DIR/target/ebpf"

echo "Building eBPF programs..."

# Ensure output directory exists
mkdir -p "$OUTPUT_DIR"

# Build eBPF programs with nightly toolchain
cd "$EBPF_DIR"

# Check if we have the required toolchain
if ! rustup run nightly rustc --version > /dev/null 2>&1; then
    echo "Error: Rust nightly toolchain not found."
    echo "Install with: rustup toolchain install nightly"
    exit 1
fi

# Check for bpf-linker
if ! command -v bpf-linker > /dev/null 2>&1; then
    echo "Error: bpf-linker not found."
    echo "Install with: cargo install bpf-linker"
    exit 1
fi

# Build for eBPF target
# Note: -Z build-std=core is required for no_std eBPF programs
cargo +nightly build \
    --target bpfel-unknown-none \
    -Z build-std=core \
    --release

# Copy compiled eBPF object files to output directory
echo "Copying eBPF objects to $OUTPUT_DIR..."
if [ -d "target/bpfel-unknown-none/release" ]; then
    cp target/bpfel-unknown-none/release/*.o "$OUTPUT_DIR/" 2>/dev/null || true
    # For binary targets, copy the executables (they are also ELF BPF objects)
    for bin in target/bpfel-unknown-none/release/syscall-trace target/bpfel-unknown-none/release/lsm-file-open target/bpfel-unknown-none/release/span-trace; do
        if [ -f "$bin" ]; then
            cp "$bin" "$OUTPUT_DIR/$(basename $bin).o"
        fi
    done
fi

echo "eBPF build complete!"
echo "Objects in $OUTPUT_DIR:"
ls -la "$OUTPUT_DIR"/*.o 2>/dev/null || echo "  (none found)"

# Return to original directory
cd "$SCRIPT_DIR"
