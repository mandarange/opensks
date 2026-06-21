// swift-tools-version: 5.9

import PackageDescription

let package = Package(
    name: "OpenSKSStudio",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "OpenSKSStudio", targets: ["OpenSKSStudio"])
    ],
    targets: [
        .executableTarget(
            name: "OpenSKSStudio",
            path: "Sources",
            resources: [.process("Resources")]
        ),
        .testTarget(
            name: "OpenSKSStudioTests",
            dependencies: ["OpenSKSStudio"],
            path: "Tests/OpenSKSStudioTests"
        )
    ]
)
