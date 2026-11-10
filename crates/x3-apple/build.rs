//! Builds the Swift package in `swift/` (SpeechAnalyzer bridge) on macOS.
//!
//! Set `X3_SKIP_SWIFT=1` to skip the Swift build (e.g. on a Mac without the
//! macOS 26 SDK); Apple STT then reports "unsupported" at runtime.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(x3_swift)");
    println!("cargo:rerun-if-env-changed=X3_SKIP_SWIFT");
    println!("cargo:rerun-if-changed=swift/Package.swift");
    println!("cargo:rerun-if-changed=swift/Sources");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let skip = std::env::var_os("X3_SKIP_SWIFT").is_some();
    if target_os == "macos" && skip {
        println!("cargo:warning=X3_SKIP_SWIFT set: Apple SpeechAnalyzer STT disabled");
    }
    if target_os == "macos" && !skip {
        link_swift();
    }
}

#[cfg(target_os = "macos")]
fn link_swift() {
    // Minimum macOS for the Swift package: SpeechAnalyzer is macOS 26+.
    swift_rs::SwiftLinker::new("26.0")
        .with_package("X3AppleSpeech", "swift")
        .link();
    println!("cargo:rustc-link-lib=framework=Speech");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-cfg=x3_swift");
}

#[cfg(not(target_os = "macos"))]
fn link_swift() {
    // Cross-compiling to macOS from another host cannot build Swift.
    println!("cargo:warning=building x3-apple for macOS on a non-macOS host: Swift bridge skipped");
}
