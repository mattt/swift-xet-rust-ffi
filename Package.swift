// swift-tools-version: 6.0
// The swift-tools-version declares the minimum version of Swift required to build this package.

import PackageDescription

let package = Package(
    name: "Xet",
    platforms: [
        .iOS(.v13),
        .macOS(.v10_15),
    ],
    products: [
        .library(
            name: "Xet",
            targets: ["Xet"]
        )
    ],
    targets: [
        // Binary target for the Rust static library
        .binaryTarget(
            name: "SwiftXetRust",
            path: "XCFrameworks/SwiftXetRust.xcframework"
        ),
        // System library target for the UniFFI-generated FFI module
        .systemLibrary(
            name: "XetFFI",
            path: "Sources/XetFFI"
        ),
        // Swift wrapper target with UniFFI-generated bindings
        .target(
            name: "Xet",
            dependencies: ["XetFFI", "SwiftXetRust"],
            exclude: [
                "libswift_xet_rust.a",
                "libswift_xet_rust.dylib"
            ],
            linkerSettings: [
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("Security"),
            ]
        ),
        .testTarget(
            name: "XetTests",
            dependencies: ["Xet"]
        ),
    ]
)
