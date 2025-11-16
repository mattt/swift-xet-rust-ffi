#!/bin/bash
# Build XCFramework for swift-xet Rust library

set -e

# Source cargo environment if it exists
if [ -f "$HOME/.cargo/env" ]; then
    source "$HOME/.cargo/env"
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUST_DIR="$PROJECT_ROOT/Rust"
BUILD_DIR="$PROJECT_ROOT/build"
XCFRAMEWORK_DIR="$PROJECT_ROOT/XCFrameworks"

echo "🏗️  Building XCFramework for swift-xet"
echo "======================================"

# Clean previous builds
echo "Cleaning previous builds..."
rm -rf "$BUILD_DIR"
rm -rf "$XCFRAMEWORK_DIR"
mkdir -p "$BUILD_DIR"
mkdir -p "$XCFRAMEWORK_DIR"

cd "$RUST_DIR"

# Build for macOS (both architectures)
echo ""
echo "📦 Building for macOS (arm64)..."
cargo build --release --target aarch64-apple-darwin

echo "📦 Building for macOS (x86_64)..."
cargo build --release --target x86_64-apple-darwin

# Build for iOS
echo ""
echo "📦 Building for iOS (arm64)..."
cargo build --release --target aarch64-apple-ios

# Build for iOS Simulator
echo ""
echo "📦 Building for iOS Simulator (arm64)..."
cargo build --release --target aarch64-apple-ios-sim

echo "📦 Building for iOS Simulator (x86_64)..."
cargo build --release --target x86_64-apple-ios

# Create universal binaries
echo ""
echo "🔨 Creating universal binaries..."

# macOS universal
lipo -create \
    "target/aarch64-apple-darwin/release/libswift_xet_rust.a" \
    "target/x86_64-apple-darwin/release/libswift_xet_rust.a" \
    -output "$BUILD_DIR/libswift_xet_rust_macos.a"

# iOS Simulator universal
lipo -create \
    "target/aarch64-apple-ios-sim/release/libswift_xet_rust.a" \
    "target/x86_64-apple-ios/release/libswift_xet_rust.a" \
    -output "$BUILD_DIR/libswift_xet_rust_ios_sim.a"

# iOS (single arch)
cp "target/aarch64-apple-ios/release/libswift_xet_rust.a" \
   "$BUILD_DIR/libswift_xet_rust_ios.a"

# Create XCFramework
echo ""
echo "📦 Creating XCFramework..."
xcodebuild -create-xcframework \
    -library "$BUILD_DIR/libswift_xet_rust_macos.a" \
    -library "$BUILD_DIR/libswift_xet_rust_ios_sim.a" \
    -library "$BUILD_DIR/libswift_xet_rust_ios.a" \
    -output "$XCFRAMEWORK_DIR/SwiftXetRust.xcframework"

echo ""
echo "✅ XCFramework built successfully!"
echo "   Location: $XCFRAMEWORK_DIR/SwiftXetRust.xcframework"
echo ""
echo "📊 Framework contents:"
find "$XCFRAMEWORK_DIR/SwiftXetRust.xcframework" -name "*.a" | while read -r lib; do
    echo "  $lib"
    file "$lib"
done
