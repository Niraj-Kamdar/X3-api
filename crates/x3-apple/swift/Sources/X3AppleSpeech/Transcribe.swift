// SpeechAnalyzer + SpeechTranscriber bridge (macOS 26+). UNVERIFIED: written
// without a Mac; must be built and run on macOS 26 with Xcode 26+.
//
// Exposes a tiny C ABI so Rust needs no Swift-specific types:
//   char *x3_apple_transcribe(const char *path, const char *locale_or_null, bool words)
//   bool  x3_apple_stt_available(void)
//   void  x3_apple_free(char *)
// The result is a JSON document matching `SwiftTranscript` in src/stt.rs.
//
// Never use SFSpeechRecognizer here: it can send audio to Apple's servers.
// SpeechTranscriber runs on-device with OS-managed models.
//
// TODO(mac): verify each API below against the macOS 26 SDK:
//   - SpeechTranscriber(locale:transcriptionOptions:reportingOptions:attributeOptions:)
//   - SpeechTranscriber.supportedLocale(equivalentTo:)
//   - SpeechTranscriber.isAvailable
//   - AssetInventory.assetInstallationRequest(supporting:) / downloadAndInstall()
//   - SpeechAnalyzer(modules:), analyzeSequence(from: AVAudioFile),
//     finalizeAndFinish(through:), cancelAndFinishNow()
//   - result.text runs carrying `audioTimeRange` when attributeOptions
//     contains .audioTimeRange
//   - result.range (CMTimeRange) for segment timing

import AVFoundation
import CoreMedia
import Foundation
import Speech

struct WordOut: Codable {
    let word: String
    let start: Double
    let end: Double
}

struct SegmentOut: Codable {
    let start: Double
    let end: Double
    let text: String
}

struct ResultOut: Codable {
    var text: String = ""
    var language: String? = nil
    var duration: Double? = nil
    var segments: [SegmentOut] = []
    var words: [WordOut]? = nil
    var error: String? = nil
}

enum X3SpeechError: Error, CustomStringConvertible {
    case unsupportedLocale(String)
    case unavailable

    var description: String {
        switch self {
        case .unsupportedLocale(let id): return "locale '\(id)' is not supported by SpeechTranscriber"
        case .unavailable: return "SpeechTranscriber is not available on this Mac"
        }
    }
}

final class Box<T>: @unchecked Sendable {
    var value: T
    init(_ value: T) { self.value = value }
}

@available(macOS 26.0, *)
func transcribeFile(path: String, localeId: String?, wantWords: Bool) async throws -> ResultOut {
    guard SpeechTranscriber.isAvailable else { throw X3SpeechError.unavailable }

    let requested = localeId.map { Locale(identifier: $0) } ?? Locale.current
    guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: requested) else {
        throw X3SpeechError.unsupportedLocale(requested.identifier)
    }

    let transcriber = SpeechTranscriber(
        locale: locale,
        transcriptionOptions: [],
        reportingOptions: [],
        attributeOptions: [.audioTimeRange]
    )

    // Models are OS-managed; the first use of a locale may trigger a system
    // asset download.
    if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
        try await request.downloadAndInstall()
    }

    let analyzer = SpeechAnalyzer(modules: [transcriber])
    let file = try AVAudioFile(forReading: URL(fileURLWithPath: path))
    let duration = Double(file.length) / file.processingFormat.sampleRate

    let collector = Task { () -> ([SegmentOut], [WordOut]) in
        var segments: [SegmentOut] = []
        var words: [WordOut] = []
        for try await result in transcriber.results {
            guard result.isFinal else { continue }
            let text = String(result.text.characters).trimmingCharacters(in: .whitespacesAndNewlines)
            if text.isEmpty { continue }
            let start = result.range.start.seconds
            let end = CMTimeRangeGetEnd(result.range).seconds
            segments.append(SegmentOut(start: start, end: end, text: text))
            if wantWords {
                for run in result.text.runs {
                    guard let range = run.audioTimeRange else { continue }
                    let w = String(result.text[run.range].characters)
                        .trimmingCharacters(in: .whitespacesAndNewlines)
                    if w.isEmpty { continue }
                    words.append(WordOut(word: w, start: range.start.seconds,
                                         end: CMTimeRangeGetEnd(range).seconds))
                }
            }
        }
        return (segments, words)
    }

    if let last = try await analyzer.analyzeSequence(from: file) {
        try await analyzer.finalizeAndFinish(through: last)
    } else {
        await analyzer.cancelAndFinishNow()
    }

    let (segments, words) = try await collector.value
    var out = ResultOut()
    out.text = segments.map { $0.text }.joined(separator: " ")
    out.language = locale.identifier
    out.duration = duration
    out.segments = segments
    out.words = wantWords ? words : nil
    return out
}

private func encode(_ out: ResultOut) -> UnsafeMutablePointer<CChar>? {
    let json = (try? JSONEncoder().encode(out)).flatMap { String(data: $0, encoding: .utf8) }
        ?? "{\"text\":\"\",\"segments\":[],\"error\":\"failed to encode result\"}"
    return strdup(json)
}

@_cdecl("x3_apple_transcribe")
public func x3_apple_transcribe(
    _ path: UnsafePointer<CChar>,
    _ locale: UnsafePointer<CChar>?,
    _ words: Bool
) -> UnsafeMutablePointer<CChar>? {
    let p = String(cString: path)
    let l = locale.map { String(cString: $0) }
    guard #available(macOS 26.0, *) else {
        var out = ResultOut()
        out.error = "SpeechAnalyzer requires macOS 26 or later"
        return encode(out)
    }
    let box = Box(ResultOut())
    let done = DispatchSemaphore(value: 0)
    // Called from a Rust blocking thread (never the main thread), so waiting
    // on a semaphore here cannot deadlock the main run loop.
    Task.detached {
        do {
            box.value = try await transcribeFile(path: p, localeId: l, wantWords: words)
        } catch {
            var out = ResultOut()
            out.error = "\(error)"
            box.value = out
        }
        done.signal()
    }
    done.wait()
    return encode(box.value)
}

@_cdecl("x3_apple_stt_available")
public func x3_apple_stt_available() -> Bool {
    if #available(macOS 26.0, *) {
        return SpeechTranscriber.isAvailable
    }
    return false
}

@_cdecl("x3_apple_free")
public func x3_apple_free(_ ptr: UnsafeMutablePointer<CChar>?) {
    free(ptr)
}
