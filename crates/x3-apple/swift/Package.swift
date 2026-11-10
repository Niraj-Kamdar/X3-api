// swift-tools-version:6.2
// Swift bridge for Apple's SpeechAnalyzer / SpeechTranscriber (macOS 26+).
// Built by ../build.rs via swift-rs and linked statically into x3-apple.
import PackageDescription

let package = Package(
    name: "X3AppleSpeech",
    platforms: [.macOS("26.0")],
    products: [
        .library(name: "X3AppleSpeech", type: .static, targets: ["X3AppleSpeech"]),
    ],
    targets: [
        .target(
            name: "X3AppleSpeech",
            path: "Sources/X3AppleSpeech",
            swiftSettings: [.swiftLanguageMode(.v5)]
        ),
    ]
)
