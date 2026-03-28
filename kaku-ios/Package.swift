// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "Kaku",
    platforms: [.iOS(.v16)],
    products: [
        .library(name: "Kaku", targets: ["Kaku"]),
    ],
    dependencies: [
        .package(
            url: "https://github.com/migueldeicaza/SwiftTerm",
            from: "1.2.3"
        ),
    ],
    targets: [
        .target(
            name: "Kaku",
            dependencies: ["SwiftTerm"],
            path: "Sources/Kaku"
        ),
    ]
)
