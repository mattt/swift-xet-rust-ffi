# swift-xet

[Xet](https://huggingface.co/docs/hub/en/xet/index)
is a storage system for large binary files that uses chunk-level deduplication.
Hugging Face uses Xet so users can download only the files they need 
without cloning the entire repository history.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="Assets/xet-speed-dark.gif">
  <source media="(prefers-color-scheme: light)" srcset="Assets/xet-speed.gif">
  <img alt="XET vs LFS">
</picture>

This project provides Swift bindings to
the [xet-core](https://github.com/huggingface/xet-core) Rust crate
using [UniFFI](https://mozilla.github.io/uniffi-rs/).

> [!WARNING]  
> This project is under active development, and not ready for production use.


## Requirements

- Swift 6.0+ / Xcode 16+
- iOS 13+ / macOS 10.15+

## Installation

### Swift Package Manager

Add the following dependency to your `Package.swift` file:

```swift
dependencies: [
    .package(url: "https://github.com/mattt/swift-xet.git", branch: "main")
],
targets: [
    .target(
        name: "YourTarget",
        dependencies: [
            .product(name: "Xet", package: "swift-xet")
        ]
    )
]
```

### Xcode

1. In Xcode, select **File** → **Add Package Dependencies...**
2. Enter the repository URL: `https://github.com/mattt/swift-xet.git`
3. Select the version you want to use
4. Add the `Xet` library to your target

## Usage

### Basic Example

```swift
import Xet

let xet = try XetClient()

guard let fileInfo = try xet.getFileInfo(
    repo: "Qwen/Qwen3-0.6B",
    path: "tokenizer.json",
    revision: "main"
) else {
    fatalError("Pointer file missing Xet metadata")
}

let jwt = try xet.getCasJwt(
    repo: "Qwen/Qwen3-0.6B",
    revision: "main",
    isUpload: false
)

let downloads = try xet.downloadFiles(
    fileInfos: [fileInfo],
    destinationDir: FileManager.default.temporaryDirectory.path,
    jwtInfo: jwt
)
print("Downloaded blobs: \(downloads)")
```

### Authentication

To use authenticated requests, create a client with a token:

```swift
let xet = try XetClient.withToken(token: "your-hf-token")
```

### High-performance downloads

`XetClient` intentionally exposes only the primitives needed for high-performance CAS transfers:

- `getFileInfo` reads pointer files to extract the Xet content hash + size.
- `getCasJwt` obtains the short-lived CAS JWT required to talk to the storage backend.
- `downloadFiles` performs the actual chunked, parallel download using the same data client that powers `hf_transfer`.

If the Hub response doesn’t advertise Xet metadata the call fails.
There is no transparent HTTP fallback,
so you always know you’re getting the fast path.

## Development

### Prerequisites

- **Rust toolchain**:
  Install via [rustup](https://rustup.rs/)
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```
- **Swift toolchain**: 
  Download [Xcode](https://developer.apple.com/xcode/) or install with [swiftly](https://www.swift.org/install/)
  ```bash
  curl -O https://download.swift.org/swiftly/darwin/swiftly.pkg && \
  installer -pkg swiftly.pkg -target CurrentUserHomeDirectory && \
  ~/.swiftly/bin/swiftly init --quiet-shell-followup && \
  . "${SWIFTLY_HOME_DIR:-$HOME/.swiftly}/env.sh" && \
  hash -r
  ```

### FFI Interface Definition

The FFI interface is defined using UniFFI's 
[Interface Definition Language (UDL)](https://mozilla.github.io/uniffi-rs/0.27/udl_file_spec.html):

- **Interface Definition**: `Rust/src/swift_xet_rust.udl` - Declares the types, functions, and errors exposed to Swift
- **Implementation**: `Rust/src/lib.rs` - Implements the Rust side of the FFI bindings

### Generating Bindings

To generate the Swift bindings from the Rust code:

```bash
./Scripts/generate-bindings.sh
```

> [!NOTE]
> The first build will compile the Rust library,
> which may take a few minutes. 
> Subsequent builds are incremental and much faster.

This script performs the following steps:

1. **Builds the Rust library** (`libswift_xet_rust.a`) for the host platform
2. **Runs the custom UniFFI generator** located in `Rust/uniffi-gen/` to process the UDL file
3. **Generates Swift files** and places them in `Sources/Xet/`
4. **Generates C FFI headers** and places them in `Sources/XetFFI/`

The custom UniFFI generator in `Rust/uniffi-gen/` is based on the `uniffi_bindgen` crate and tailored for this project's specific needs.

### Swift Package Structure

The Swift package is defined in `Package.swift` and consists of two main targets:

**1. `Xet` Target (Public Module)**

The public-facing Swift module containing the generated UniFFI bindings and convenience wrappers:

```swift
.target(
    name: "Xet",
    dependencies: ["XetFFI"],
    path: "Sources/Xet"
)
```

This is the module that Swift projects import and use.

**2. `XetFFI` Target (Internal System Library)**

An internal system library target that exposes the C FFI headers:

```swift
.systemLibrary(
    name: "XetFFI",
    path: "Sources/XetFFI",
    pkgConfig: "swift-xet-rust"
)
```

This target bridges the generated Swift code to the underlying Rust library via C FFI. 
It is an internal dependency and not directly used by consumers.

### Building the Package

Once bindings are generated, build the Swift package:

```bash
swift build
```

Run tests:

```bash
swift test
```

### Adding New Xet Functionality

The current implementation provides a complete foundation ready for actual Xet API integration:

1. **Expand the UDL Interface** (`Rust/src/swift_xet_rust.udl`):
   - Add new types, functions, and errors following UniFFI conventions
   - Ensure types are UniFFI-compatible (primitives, Vec, HashMap, Option, Result, custom structs/enums)

2. **Implement in Rust** (`Rust/src/lib.rs`):
   - Import and wrap `hub_client` crate functionality
   - Add proper error handling with `Result<T, XetError>`
   - Handle async operations (UniFFI supports async with proper setup)

3. **Regenerate Bindings**:
   ```bash
   ./Scripts/generate-bindings.sh
   ```

4. **Add Swift Convenience APIs** (optional):
   - Create idiomatic Swift wrappers in `Sources/Xet/` if needed
   - Follow Swift naming conventions

5. **Test**:
   ```bash
   swift build
   swift test
   ```

## Troubleshooting

### Build Errors

**Rust not found or targets missing:**
```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Add required targets for cross-compilation
rustup target add aarch64-apple-ios
rustup target add x86_64-apple-ios
rustup target add aarch64-apple-darwin
rustup target add x86_64-apple-darwin
```

**UniFFI generation errors:**
- Ensure `uniffi` and `uniffi_build` are in `Rust/Cargo.toml` dependencies
- Check that the UDL file syntax is correct
- Verify the custom generator in `Rust/uniffi-gen/` builds successfully

**Dependency resolution fails:**
- Verify the xet-core repository path in `Rust/Cargo.toml` is correct
- Check that the `hub_client` crate exists in the xet-core workspace
- Try updating dependencies with `cargo update`

### Runtime Errors

**"Cannot find type 'XetClient' in scope":**

Bindings haven't been generated yet. Run:
```bash
./Scripts/generate-bindings.sh
```

**Linking errors:**
- Ensure the Rust library was built for the correct architecture
- Check that `Sources/XetFFI/module.modulemap` correctly points to headers
- Verify library search paths in Swift package configuration

### Testing Without Full Integration

To test the Rust side independently:

```bash
cd Rust
cargo build
cargo test
```

## License

This project is available under the MIT license.
See the LICENSE file for more info.
