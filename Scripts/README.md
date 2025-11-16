# swift-xet Build Scripts

This directory contains scripts for building and packaging the swift-xet Rust library.

## Scripts

### `generate-bindings.sh`

Generates UniFFI Swift bindings from the Rust library for local development.

```bash
./Scripts/generate-bindings.sh
```

**What it does:**
- Builds the Rust library in release mode for the current architecture
- Generates Swift bindings using UniFFI
- Copies the dynamic library (.dylib) and static library (.a) to `Sources/Xet/`
- Copies FFI headers to `Sources/XetFFI/`

**When to use:** During local development when you've made changes to the Rust code and want to test them in Swift.

### `build-xcframework.sh`

Builds a multi-platform XCFramework for distribution.

```bash
./Scripts/build-xcframework.sh
```

**What it does:**
- Builds the Rust library for all supported platforms:
  - macOS (arm64 + x86_64 universal binary)
  - iOS (arm64)
  - iOS Simulator (arm64 + x86_64 universal binary)
- Creates an XCFramework containing all platform binaries
- Outputs to `XCFrameworks/SwiftXetRust.xcframework`

**When to use:** 
- Before committing changes that affect the Rust library
- When preparing a release
- When the XCFramework needs to be updated for CI/CD or distribution

## Requirements

- Xcode Command Line Tools
- Rust toolchain with the following targets installed:
  ```bash
  rustup target add aarch64-apple-darwin
  rustup target add x86_64-apple-darwin
  rustup target add aarch64-apple-ios
  rustup target add aarch64-apple-ios-sim
  rustup target add x86_64-apple-ios
  ```

## Build Artifacts

- **Local development:** `Sources/Xet/libswift_xet_rust.dylib`
- **Distribution:** `XCFrameworks/SwiftXetRust.xcframework/`

## Notes

- The Swift package (`Package.swift`) uses the XCFramework for distribution
- Local development uses the dylib for faster iteration
- The XCFramework should be rebuilt after any Rust code changes before committing
