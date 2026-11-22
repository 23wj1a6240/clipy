// Live SpeechAnalyzer transcription for Local mode (macOS 26 and later).
//
// `apple_stream.m` drives `SFSpeechRecognizer`, the API Apple built for short
// dictation. macOS 26 added `SpeechAnalyzer` + `SpeechTranscriber`: a newer
// on-device model built for long-form audio, with per-token audio time ranges
// and no per-task length limit. The model is a system asset managed by the OS,
// so it costs no download in Clipy's bundle and no model memory in Clipy's
// process.
//
// This file exposes exactly the C surface `apple_stream.m` does (open, append,
// session_seconds, rotate, finish, close, free) under a `clipy_sa_` prefix, so
// `live_stt.rs` can try this engine first and fall back to the old one without
// a second feeder. The JSON shapes are the same: `{"segments":[...]}` with
// stream-relative `startMs`/`endMs`, plus `"warning"` when the session's window
// is not fully covered, or `{"error":...}` when a call could not run.
//
// Open never waits for a model download. When the system language's asset is
// not installed yet, open fails (so this take uses SFSpeech) and the download
// is requested in the background for the next take. No audio is involved in
// that request.
//
// Threading: `append` is called from the feeder thread, `rotate`/`finish` from
// the feeder or the local processing thread. One lock covers the session
// object. Async work runs on detached tasks; the C entry points wait on a
// semaphore, which is safe because the callers are plain threads, never the
// Swift cooperative pool.
//
// The SDK guard: SpeechAnalyzer ships in the macOS 26 SDK (Swift 6.2). On an
// older toolchain this compiles to stubs whose open always fails, so the app
// still builds and simply keeps using SFSpeech.

import AVFoundation
import CoreMedia
import Foundation
import Speech

private func copyOut(_ value: String) -> UnsafeMutablePointer<CChar>? {
    strdup(value)
}

private func jsonOut(_ object: [String: Any]) -> UnsafeMutablePointer<CChar>? {
    guard let data = try? JSONSerialization.data(withJSONObject: object),
          let text = String(data: data, encoding: .utf8)
    else { return copyOut("{\"error\":\"Live transcription produced unreadable output.\"}") }
    return copyOut(text)
}

private func errorOut(_ message: String) -> UnsafeMutablePointer<CChar>? {
    jsonOut(["error": message])
}

@_cdecl("clipy_sa_free")
public func clipy_sa_free(_ value: UnsafeMutablePointer<CChar>?) {
    free(value)
}

#if compiler(>=6.2)

/// Mirrors `ClipySpeechConfigurationError` in `speech_permissions.h`: TCC can
/// abort an unbundled executable that touches Speech, which nothing can catch.
private func speechConfigurationError() -> String? {
    let bundle = Bundle.main
    if bundle.bundleURL.pathExtension != "app" {
        return "Open Clipy from its application bundle to use Local mode. For development, run pnpm run tauri:dev:app from desktop/."
    }
    let description = bundle.object(forInfoDictionaryKey: "NSSpeechRecognitionUsageDescription") as? String
    if description?.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ?? true {
        return "This Clipy build is missing its Speech Recognition privacy description. Reinstall Clipy or rebuild the application bundle before using Local mode."
    }
    return nil
}

private struct OpenFailure: Error {
    let message: String
}

/// Holds an async result for a thread that is blocked on a semaphore. Once the
/// waiter gives up, a value that arrives later is refused so it can be cleaned
/// up instead of silently dropped.
private final class Outcome<T>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: T?
    private var abandoned = false

    /// False when the waiter has already given up on this value.
    func set(_ value: T) -> Bool {
        lock.lock(); defer { lock.unlock() }
        if abandoned { return false }
        stored = value
        return true
    }

    /// The value if it has arrived, even just after a timeout; otherwise marks
    /// the outcome abandoned so a later `set` is refused.
    func takeOrAbandon() -> T? {
        lock.lock(); defer { lock.unlock() }
        if stored == nil { abandoned = true }
        return stored
    }
}

/// Runs `body` on a detached task and waits for it. Returns nil on timeout;
/// the task keeps running, and if it finishes later its value goes to
/// `discardLate` so nothing it owns is left running.
private func blockOn<T>(
    seconds: Double?,
    discardLate: (@Sendable (T) -> Void)? = nil,
    _ body: @escaping @Sendable () async -> T
) -> T? {
    let done = DispatchSemaphore(value: 0)
    let outcome = Outcome<T>()
    Task.detached(priority: .userInitiated) {
        let value = await body()
        if outcome.set(value) {
            done.signal()
        } else {
            discardLate?(value)
        }
    }
    if let seconds {
        _ = done.wait(timeout: .now() + seconds)
    } else {
        done.wait()
    }
    return outcome.takeOrAbandon()
}

private let gLock = NSLock()
/// `AnalyzerSession` when open. Typed as `AnyObject` because the class only
/// exists on macOS 26.
private var gSession: AnyObject?
/// Seconds of audio handed to analyzers that have already been harvested, so a
/// segment's stream time is this plus its own in-session time.
private var gBaseSeconds: Double = 0
private var gAssetRequestStarted = false

@available(macOS 26.0, *)
private final class AnalyzerSession: @unchecked Sendable {
    let analyzer: SpeechAnalyzer
    let input: AsyncStream<AnalyzerInput>.Continuation
    let micFormat: AVAudioFormat
    let analyzerFormat: AVAudioFormat
    let converter: AVAudioConverter?
    var resultsTask: Task<Void, Never>?

    private let lock = NSLock()
    private var segments: [[String: Any]] = []
    private var failure: Error?
    private var ended = false
    private(set) var seconds: Double = 0

    init(analyzer: SpeechAnalyzer, input: AsyncStream<AnalyzerInput>.Continuation,
         micFormat: AVAudioFormat, analyzerFormat: AVAudioFormat, converter: AVAudioConverter?) {
        self.analyzer = analyzer
        self.input = input
        self.micFormat = micFormat
        self.analyzerFormat = analyzerFormat
        self.converter = converter
    }

    var hasEnded: Bool { lock.lock(); defer { lock.unlock() }; return ended }

    func markEnded() { lock.lock(); ended = true; lock.unlock() }

    /// Cancellation is recorded too: `finalizeAndFinishThroughEndOfInput()`
    /// throws it when analysis stopped before consuming all input, so the
    /// window is partial and must go back to the file path. The abort path
    /// (`cancel`) never harvests, so its own cancellation is never reported.
    func fail(_ error: Error) {
        lock.lock()
        if failure == nil {
            failure = error is CancellationError
                ? OpenFailure(message: "SpeechAnalyzer finished before transcribing all audio.")
                : error
        }
        ended = true
        lock.unlock()
    }

    /// Turns one final result into segments on this session's clock. Each
    /// attributed run carries the audio range of the token it covers; a run
    /// without one (punctuation can arrive that way) joins the token before it.
    func collect(_ result: SpeechTranscriber.Result) {
        var pieces: [(range: CMTimeRange, text: String)] = []
        let text = result.text
        for run in text.runs {
            let piece = String(text[run.range].characters)
            if let range = run.audioTimeRange {
                if let last = pieces.last, CMTimeRangeEqual(last.range, range) {
                    pieces[pieces.count - 1].text += piece
                } else {
                    pieces.append((range, piece))
                }
            } else if !pieces.isEmpty {
                pieces[pieces.count - 1].text += piece
            } else {
                pieces.append((result.range, piece))
            }
        }
        var out: [[String: Any]] = []
        for piece in pieces {
            let words = piece.text.trimmingCharacters(in: .whitespacesAndNewlines)
            let start = piece.range.start.seconds
            let end = piece.range.end.seconds
            guard !words.isEmpty, start.isFinite, end.isFinite else { continue }
            let startSeconds = max(0, start)
            out.append([
                "startMs": UInt64(startSeconds * 1000),
                "endMs": UInt64(max(startSeconds, end) * 1000),
                "text": words,
            ])
        }
        lock.lock()
        segments.append(contentsOf: out)
        lock.unlock()
    }

    /// Converts and queues interleaved float frames. Returns 0 when accepted,
    /// 1 when the analyzer has already ended and the caller should rotate.
    /// Never waits on recognition: `yield` only buffers the input.
    func append(_ samples: UnsafePointer<Float>, frames: AVAudioFrameCount) -> Int32 {
        if hasEnded { return 1 }
        guard let buffer = AVAudioPCMBuffer(pcmFormat: micFormat, frameCapacity: frames),
              let destination = buffer.floatChannelData?[0]
        else { return -1 }
        buffer.frameLength = frames
        memcpy(destination, samples, Int(frames) * Int(micFormat.channelCount) * MemoryLayout<Float>.size)
        guard let converter else {
            input.yield(AnalyzerInput(buffer: buffer))
            seconds += Double(frames) / micFormat.sampleRate
            return 0
        }
        let ratio = analyzerFormat.sampleRate / micFormat.sampleRate
        let capacity = AVAudioFrameCount((Double(frames) * ratio).rounded(.up)) + 64
        guard let converted = AVAudioPCMBuffer(pcmFormat: analyzerFormat, frameCapacity: capacity) else { return -1 }
        var supplied = false
        var conversionError: NSError?
        // `.noDataNow` (not `.endOfStream`) keeps the resampler's history, so
        // consecutive callbacks join without a click at every boundary.
        let status = converter.convert(to: converted, error: &conversionError) { _, inputStatus in
            if supplied {
                inputStatus.pointee = .noDataNow
                return nil
            }
            supplied = true
            inputStatus.pointee = .haveData
            return buffer
        }
        if status == .error {
            fail(conversionError ?? OpenFailure(message: "Audio conversion for live transcription failed."))
            return 1
        }
        if converted.frameLength > 0 {
            input.yield(AnalyzerInput(buffer: converted))
        }
        seconds += Double(frames) / micFormat.sampleRate
        return 0
    }

    /// Drains audio the resampler is still holding, so the end of the last
    /// word reaches the analyzer before its input is finished.
    private func flushConverter() {
        guard let converter else { return }
        while true {
            guard let out = AVAudioPCMBuffer(pcmFormat: analyzerFormat, frameCapacity: 4096) else { return }
            var conversionError: NSError?
            let status = converter.convert(to: out, error: &conversionError) { _, inputStatus in
                inputStatus.pointee = .endOfStream
                return nil
            }
            if out.frameLength > 0 {
                input.yield(AnalyzerInput(buffer: out))
            }
            if status == .error {
                fail(conversionError ?? OpenFailure(message: "Audio conversion for live transcription failed."))
                return
            }
            if status != .haveData { return }
        }
    }

    /// Ends input, waits for the last results, and returns stream-relative JSON
    /// fields. `base` is the stream time this session started at.
    func harvest(base: Double) -> [String: Any] {
        flushConverter()
        input.finish()
        // The analyzer has already consumed this audio, so what remains is
        // finalising the last utterance. The bound only exists so a wedged
        // speech service cannot hang Stop, matching apple_stream.m.
        let analyzer = self.analyzer
        let resultsTask = self.resultsTask
        let completed = blockOn(seconds: 30) { [self] in
            do {
                try await analyzer.finalizeAndFinishThroughEndOfInput()
            } catch {
                self.fail(error)
            }
            await resultsTask?.value
            return true
        } ?? false
        if !completed {
            Task.detached { await analyzer.cancelAndFinishNow() }
        }
        lock.lock()
        let collected = segments
        let failure = self.failure
        lock.unlock()
        let shifted: [[String: Any]] = collected.map { segment in
            var copy = segment
            copy["startMs"] = (segment["startMs"] as? UInt64 ?? 0) + UInt64(base * 1000)
            copy["endMs"] = (segment["endMs"] as? UInt64 ?? 0) + UInt64(base * 1000)
            return copy
        }
        var out: [String: Any] = ["segments": shifted]
        // As in apple_stream.m, a warning means "this window is not fully
        // covered" and the Rust side hands the window back to the file path.
        if let failure {
            out["warning"] = failure.localizedDescription
        } else if !completed {
            out["warning"] = "Live transcription timed out waiting for a result."
        }
        return out
    }

    func cancel() {
        input.finish()
        resultsTask?.cancel()
        let analyzer = self.analyzer
        Task.detached { await analyzer.cancelAndFinishNow() }
    }

    static func open(sampleRate: Double, channels: UInt32) async -> Result<AnalyzerSession, OpenFailure> {
        guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale.current) else {
            return .failure(OpenFailure(message: "SpeechAnalyzer does not support this system language."))
        }
        let installed = await SpeechTranscriber.installedLocales
        let wanted = locale.identifier(.bcp47)
        guard installed.contains(where: { $0.identifier(.bcp47) == wanted }) else {
            requestAssetsInBackground(locale: locale)
            return .failure(OpenFailure(message: "SpeechAnalyzer assets for \(wanted) are not installed yet."))
        }
        let transcriber = SpeechTranscriber(
            locale: locale,
            transcriptionOptions: [],
            reportingOptions: [],
            attributeOptions: [.audioTimeRange]
        )
        guard let analyzerFormat = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
            return .failure(OpenFailure(message: "SpeechAnalyzer reported no usable audio format."))
        }
        guard let micFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: sampleRate,
            channels: AVAudioChannelCount(channels),
            interleaved: true
        ) else {
            return .failure(OpenFailure(message: "Could not describe the microphone format for live transcription."))
        }
        var converter: AVAudioConverter?
        if micFormat != analyzerFormat {
            guard let made = AVAudioConverter(from: micFormat, to: analyzerFormat) else {
                return .failure(OpenFailure(message: "Could not convert the microphone format for SpeechAnalyzer."))
            }
            converter = made
        }
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        let (stream, continuation) = AsyncStream.makeStream(of: AnalyzerInput.self)
        let session = AnalyzerSession(
            analyzer: analyzer, input: continuation,
            micFormat: micFormat, analyzerFormat: analyzerFormat, converter: converter
        )
        // Only final results are requested, so every result is kept: the same
        // "one final per utterance" shape apple_stream.m's delegate collects.
        session.resultsTask = Task.detached(priority: .userInitiated) {
            do {
                for try await result in transcriber.results where result.isFinal {
                    session.collect(result)
                }
                session.markEnded()
            } catch {
                session.fail(error)
            }
        }
        do {
            try await analyzer.start(inputSequence: stream)
        } catch {
            session.cancel()
            return .failure(OpenFailure(message: "SpeechAnalyzer could not start: \(error.localizedDescription)"))
        }
        return .success(session)
    }

    /// Asks the OS for the language model once per launch, without waiting.
    /// The next take can then use SpeechAnalyzer.
    private static func requestAssetsInBackground(locale: Locale) {
        gLock.lock()
        let started = gAssetRequestStarted
        gAssetRequestStarted = true
        gLock.unlock()
        if started { return }
        Task.detached(priority: .utility) {
            let transcriber = SpeechTranscriber(
                locale: locale,
                transcriptionOptions: [],
                reportingOptions: [],
                attributeOptions: [.audioTimeRange]
            )
            if let request = try? await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                try? await request.downloadAndInstall()
            }
        }
    }
}

/// Opens a session, waiting at most 15 s. A session that finishes opening after
/// the wait gave up is cancelled, so a slow speech service can't leave one
/// analyzer running per attempted take.
@available(macOS 26.0, *)
private func openSession(sampleRate: Double, channels: UInt32) -> Result<AnalyzerSession, OpenFailure>? {
    blockOn(seconds: 15, discardLate: { late in
        if case .success(let session) = late { session.cancel() }
    }) {
        await AnalyzerSession.open(sampleRate: sampleRate, channels: channels)
    }
}

/// Opens a live session for interleaved float PCM at the microphone's own
/// rate. Returns NULL on success, or a JSON `{"error":...}` string.
@_cdecl("clipy_sa_open")
public func clipy_sa_open(_ sampleRate: Double, _ channels: UInt32) -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 26.0, *) else {
        return errorOut("SpeechAnalyzer needs macOS 26 or later.")
    }
    if let message = speechConfigurationError() { return errorOut(message) }
    guard sampleRate > 0, channels > 0, channels <= 8 else {
        return errorOut("Unsupported microphone format for live transcription.")
    }
    // The same permission SFSpeech needs, so switching engines never changes
    // what Local mode asks the user for.
    guard SFSpeechRecognizer.authorizationStatus() == .authorized else {
        return errorOut("Speech Recognition permission is not granted.")
    }
    gLock.lock()
    let busy = gSession != nil
    gLock.unlock()
    if busy { return errorOut("A live transcription session is already open.") }
    guard let opened = openSession(sampleRate: sampleRate, channels: channels) else {
        return errorOut("SpeechAnalyzer took too long to start.")
    }
    switch opened {
    case .failure(let failure):
        return errorOut(failure.message)
    case .success(let session):
        gLock.lock()
        gSession = session
        gBaseSeconds = 0
        gLock.unlock()
        return nil
    }
}

/// 0 when accepted, 1 when the analyzer ended and the caller should rotate
/// before appending more, -1 when no session is open.
@_cdecl("clipy_sa_append")
public func clipy_sa_append(_ samples: UnsafePointer<Float>?, _ frames: UInt32) -> Int32 {
    guard #available(macOS 26.0, *) else { return -1 }
    guard let samples, frames > 0 else { return -1 }
    gLock.lock()
    defer { gLock.unlock() }
    guard let session = gSession as? AnalyzerSession else { return -1 }
    return session.append(samples, frames: AVAudioFrameCount(frames))
}

@_cdecl("clipy_sa_session_seconds")
public func clipy_sa_session_seconds() -> Double {
    guard #available(macOS 26.0, *) else { return -1 }
    gLock.lock()
    defer { gLock.unlock() }
    guard let session = gSession as? AnalyzerSession else { return -1 }
    return session.seconds
}

/// Harvests the current analyzer and immediately opens the next one on the
/// same format.
@_cdecl("clipy_sa_rotate")
public func clipy_sa_rotate() -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 26.0, *) else { return errorOut("No live transcription session.") }
    gLock.lock()
    defer { gLock.unlock() }
    guard let session = gSession as? AnalyzerSession else { return errorOut("No live transcription session.") }
    let harvested = session.harvest(base: gBaseSeconds)
    gBaseSeconds += session.seconds
    let rate = session.micFormat.sampleRate
    let channels = session.micFormat.channelCount
    guard let reopened = openSession(sampleRate: rate, channels: channels),
          case .success(let next) = reopened
    else {
        gSession = nil
        return errorOut("Could not continue live transcription.")
    }
    gSession = next
    return jsonOut(harvested)
}

/// Ends the session and returns its remaining segments.
@_cdecl("clipy_sa_finish")
public func clipy_sa_finish() -> UnsafeMutablePointer<CChar>? {
    guard #available(macOS 26.0, *) else { return errorOut("No live transcription session.") }
    gLock.lock()
    defer { gLock.unlock() }
    guard let session = gSession as? AnalyzerSession else { return errorOut("No live transcription session.") }
    let harvested = session.harvest(base: gBaseSeconds)
    gSession = nil
    gBaseSeconds = 0
    return jsonOut(harvested)
}

/// Abandons the session without waiting for a result (cancel or discard).
/// Safe to call with no session open.
@_cdecl("clipy_sa_close")
public func clipy_sa_close() {
    guard #available(macOS 26.0, *) else { return }
    gLock.lock()
    defer { gLock.unlock() }
    (gSession as? AnalyzerSession)?.cancel()
    gSession = nil
    gBaseSeconds = 0
}

#else

// Pre-macOS-26 SDK: no SpeechAnalyzer to link against. Open always fails, so
// `live_stt` uses SFSpeech, and every other entry point is inert.

@_cdecl("clipy_sa_open")
public func clipy_sa_open(_ sampleRate: Double, _ channels: UInt32) -> UnsafeMutablePointer<CChar>? {
    errorOut("This Clipy build was compiled without SpeechAnalyzer (needs the macOS 26 SDK).")
}

@_cdecl("clipy_sa_append")
public func clipy_sa_append(_ samples: UnsafePointer<Float>?, _ frames: UInt32) -> Int32 { -1 }

@_cdecl("clipy_sa_session_seconds")
public func clipy_sa_session_seconds() -> Double { -1 }

@_cdecl("clipy_sa_rotate")
public func clipy_sa_rotate() -> UnsafeMutablePointer<CChar>? { errorOut("No live transcription session.") }

@_cdecl("clipy_sa_finish")
public func clipy_sa_finish() -> UnsafeMutablePointer<CChar>? { errorOut("No live transcription session.") }

@_cdecl("clipy_sa_close")
public func clipy_sa_close() {}

#endif
