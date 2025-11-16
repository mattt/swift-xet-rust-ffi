#!/bin/bash
# Generate UniFFI Swift bindings from Rust code

set -e

# Source cargo environment if it exists
if [ -f "$HOME/.cargo/env" ]; then
    source "$HOME/.cargo/env"
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUST_DIR="$PROJECT_ROOT/Rust"
OUTPUT_DIR="$PROJECT_ROOT/Sources/Xet"

cd "$RUST_DIR"

echo "Building Rust library..."
cargo build --release

echo "Generating UniFFI Swift bindings..."

# Generate the bindings using the permanent uniffi-gen project
cd "$RUST_DIR/uniffi-gen"
cargo run --release -- "$RUST_DIR/src/swift_xet_rust.udl" "$OUTPUT_DIR"

cd "$RUST_DIR"

echo "Copying library artifacts..."
cp target/release/libswift_xet_rust.dylib "$OUTPUT_DIR/"
cp target/release/libswift_xet_rust.a "$OUTPUT_DIR/" 2>/dev/null || true

# Move FFI header and modulemap to the system library target
mkdir -p "$PROJECT_ROOT/Sources/XetFFI"
mv "$OUTPUT_DIR/swift_xet_rustFFI.h" "$PROJECT_ROOT/Sources/XetFFI/"
mv "$OUTPUT_DIR/swift_xet_rustFFI.modulemap" "$PROJECT_ROOT/Sources/XetFFI/module.modulemap"

echo ""
echo "✓ Successfully generated Swift bindings!"
echo "Swift bindings: $OUTPUT_DIR/swift_xet_rust.swift"
echo "FFI header: $PROJECT_ROOT/Sources/XetFFI/swift_xet_rustFFI.h"
echo "Dynamic library: $OUTPUT_DIR/libswift_xet_rust.dylib"
