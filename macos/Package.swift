// swift-tools-version: 6.0
import PackageDescription

// HostlerKit: everything of the app that is not an AppKit scene — the contract, the client,
// the OS-facing protocols with their default implementations, and the @MainActor features.
// `swift test` covers it headless; the `Hostler` app target (project.yml) links it.
// A deviation from the reference app (one executable target), so the tests need no app host.
let package = Package(
    name: "HostlerKit",
    platforms: [.macOS("14.2")],
    products: [
        .library(name: "HostlerKit", targets: ["HostlerKit"])
    ],
    targets: [
        .target(
            name: "HostlerKit",
            path: "Sources/HostlerKit",
            exclude: ["Contract/contract-schema.json"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "HostlerKitTests",
            dependencies: ["HostlerKit"],
            path: "Tests/HostlerKitTests",
            resources: [.copy("Fixtures")],
            swiftSettings: [.swiftLanguageMode(.v6)]
        )
    ]
)
