// SPDX-License-Identifier: GPL-3.0-or-later

import Combine
import CoreGraphics
import CryptoKit
import Darwin
import Foundation

struct AuthoredPESessionImage: Sendable {
    let url: URL
    let sha256: String

    fileprivate var isValid: Bool {
        url.isFileURL && sha256.utf8.count == 64 &&
            sha256.utf8.allSatisfy { (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0) }
    }
}

enum AuthoredPESessionBackend: String, CaseIterable, Sendable {
    case reference
    case metal
}

struct AuthoredPESessionConfiguration: Sendable {
    let executable: AuthoredPESessionImage
    let companions: [AuthoredPESessionImage]
    let backend: AuthoredPESessionBackend
    var deterministicClock = true

    fileprivate var images: [AuthoredPESessionImage] { [executable] + companions }

    func arguments() throws -> [String] {
        guard companions.count <= 255 else {
            throw AuthoredPESessionError("A session accepts up to 255 companion DLLs.")
        }
        guard images.allSatisfy(\.isValid) else {
            throw AuthoredPESessionError("Every guest file requires a 64-digit hexadecimal SHA-256.")
        }
        var result = [
            "--experimental", "--pe", executable.url.path, "--sha256", executable.sha256.lowercased(),
            "--graphics-backend", backend.rawValue, "--pixels",
        ]
        for companion in companions {
            result += ["--package", companion.url.path, companion.sha256.lowercased()]
        }
        if deterministicClock { result.append("--deterministic-clock") }
        return result
    }
}

struct AuthoredPESessionFrame: Equatable, Sendable {
    let index: Int
    let width: Int
    let height: Int
    let rgba: Data
    let sha256: String

    var image: CGImage? {
        guard let provider = CGDataProvider(data: rgba as CFData) else { return nil }
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent
        )
    }
}

struct AuthoredPESessionError: LocalizedError {
    let description: String

    init(_ description: String) { self.description = description }
    var errorDescription: String? { description }
}

enum AuthoredPESessionRecord: Equatable, Sendable {
    case ready
    case frame(AuthoredPESessionFrame)
    case done
}

/// The CLI writes an ASCII record per line. Buffer only one incomplete record.
struct AuthoredPESessionTranscript {
    static let maxFrames = 64
    static let maxDimension = 256
    private static let maxLine = maxDimension * maxDimension * 8 + 128
    private var line: [UInt8] = []
    private var ready = false
    private var done = false
    private var frames = 0

    mutating func append(_ bytes: UnsafeBufferPointer<UInt8>,
                         publish: (AuthoredPESessionRecord) -> Void) throws {
        for byte in bytes {
            if byte == 10 {
                let record = try parseLine()
                line.removeAll(keepingCapacity: true)
                publish(record)
            } else {
                guard !done, line.count < Self.maxLine else {
                    throw AuthoredPESessionError("The session returned an oversized or trailing record.")
                }
                line.append(byte)
            }
        }
    }

    func end() throws {
        guard line.isEmpty else {
            throw AuthoredPESessionError("The session returned an incomplete record.")
        }
    }

    private mutating func parseLine() throws -> AuthoredPESessionRecord {
        let malformed = AuthoredPESessionError("The session returned an invalid frame or completion record.")
        guard !done, let text = String(bytes: line, encoding: .ascii) else { throw malformed }
        if !ready {
            guard text == "ready" else {
                throw AuthoredPESessionError("The session did not start with a ready record.")
            }
            ready = true
            return .ready
        }
        if text == "done frames=\(frames)" {
            done = true
            return .done
        }
        let fields = text.split(separator: " ", omittingEmptySubsequences: false)
        let index = frames + 1
        guard fields.count == 6, fields[0] == "frame", fields[1] == String(index),
              index <= Self.maxFrames,
              let width = dimension(fields[2], prefix: "width="),
              let height = dimension(fields[3], prefix: "height="),
              fields[4].hasPrefix("sha256="), fields[5].hasPrefix("rgba=")
        else { throw malformed }
        let hash = fields[4].dropFirst(7)
        let pixels = fields[5].dropFirst(5).utf8
        let byteCount = width * height * 4
        guard hash.utf8.count == 64, pixels.count == byteCount * 2 else { throw malformed }
        var rgba = Data(capacity: byteCount)
        var hex = pixels.makeIterator()
        while let high = hex.next(), let low = hex.next() {
            guard let a = nibble(high), let b = nibble(low) else { throw malformed }
            rgba.append(a * 16 + b)
        }
        let digest = SHA256.hash(data: rgba).map { String(format: "%02x", $0) }.joined()
        guard hash == digest else { throw AuthoredPESessionError("The session frame SHA-256 does not match its pixels.") }
        frames = index
        return .frame(AuthoredPESessionFrame(index: index, width: width, height: height, rgba: rgba, sha256: digest))
    }

    private func dimension(_ field: Substring, prefix: String) -> Int? {
        guard field.hasPrefix(prefix), let value = Int(field.dropFirst(prefix.count)),
              (1...Self.maxDimension).contains(value), field == "\(prefix)\(value)"
        else { return nil }
        return value
    }

    private func nibble(_ ascii: UInt8) -> UInt8? {
        switch ascii {
        case 48...57: ascii - 48
        case 97...102: ascii - 87
        default: nil
        }
    }
}

private enum AuthoredPESessionEvent: Sendable {
    case record(AuthoredPESessionRecord)
    case exited(reaped: Bool, status: Int32?, problem: String?, diagnostics: String)
}

private final class SessionReadState: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false
    private var shutdownDeadline: DispatchTime?
    private var exited = false
    private var status: Int32?
    private var stoppingDiagnostics = false
    private var diagnostics = ""

    var isCancelled: Bool { lock.withLock { cancelled } }
    var deadline: DispatchTime? { lock.withLock { shutdownDeadline } }
    var didExit: Bool { lock.withLock { exited } }
    var exitStatus: Int32? { lock.withLock { status } }
    var shouldStopDiagnostics: Bool { lock.withLock { stoppingDiagnostics } }
    func cancel() { lock.withLock { cancelled = true } }
    func beginShutdown(by deadline: DispatchTime) { lock.withLock { shutdownDeadline = deadline } }
    func recordExit(_ status: Int32?) {
        lock.withLock {
            self.status = status
            exited = true
        }
    }
    func stopDiagnostics() { lock.withLock { stoppingDiagnostics = true } }
    func storeDiagnostics(_ value: String) { lock.withLock { diagnostics = value } }
    func readDiagnostics() -> String { lock.withLock { diagnostics } }
}

/// All input operations belong to the model's main actor. Each read descriptor
/// has one background owner and closes on that same thread.
@MainActor
private final class AuthoredPESessionProcess {
    let events: AsyncStream<AuthoredPESessionEvent>
    private let input: FileHandle
    private let shutdownGrace: TimeInterval
    private let readState = SessionReadState()

    init(executable: URL, configuration: AuthoredPESessionConfiguration, shutdownGrace: TimeInterval) throws {
        let process = Process()
        process.executableURL = executable
        process.arguments = try configuration.arguments()
        let stdin = Pipe()
        let stdout = Pipe()
        let stderr = Pipe()
        let input = stdin.fileHandleForWriting
        guard fcntl(input.fileDescriptor, F_SETNOSIGPIPE, 1) == 0 else {
            throw AuthoredPESessionError("Could not configure the session input pipe.")
        }
        process.standardInput = stdin
        process.standardOutput = stdout
        process.standardError = stderr

        let scoped = configuration.images.map(\.url).filter { $0.startAccessingSecurityScopedResource() }
        let state = readState
        let exit = DispatchSemaphore(value: 0)
        process.terminationHandler = { child in
            let status = child.terminationReason == .exit ? child.terminationStatus : nil
            for url in scoped { url.stopAccessingSecurityScopedResource() }
            state.recordExit(status)
            exit.signal()
        }
        do {
            try process.run()
        } catch {
            for url in scoped { url.stopAccessingSecurityScopedResource() }
            throw error
        }
        self.input = input
        self.shutdownGrace = shutdownGrace
        try? stdin.fileHandleForReading.close()
        try? stdout.fileHandleForWriting.close()
        try? stderr.fileHandleForWriting.close()

        let (events, continuation) = AsyncStream<AuthoredPESessionEvent>.makeStream()
        self.events = events
        let output = stdout.fileHandleForReading
        let errors = stderr.fileHandleForReading
        let drain = DispatchGroup()
        drain.enter()
        DispatchQueue.global(qos: .userInitiated).async {
            state.storeDiagnostics(Self.collectDiagnostics(errors, state: state))
            try? errors.close()
            drain.leave()
        }
        DispatchQueue.global(qos: .userInitiated).async {
            let problem = Self.readRecords(output, state: state, continuation: continuation)
            try? output.close()
            let exitProblem = Self.awaitExit(
                process, semaphore: exit, deadline: state.deadline ?? .now() + shutdownGrace
            )
            if drain.wait(timeout: .now() + .milliseconds(250)) == .timedOut {
                state.stopDiagnostics()
                _ = drain.wait(timeout: .now() + .milliseconds(250))
            }
            let reaped = state.didExit
            continuation.yield(.exited(
                reaped: reaped, status: state.exitStatus, problem: reaped ? problem ?? exitProblem : exitProblem,
                diagnostics: state.readDiagnostics()
            ))
            continuation.finish()
        }
    }

    func send(_ value: UInt32) throws {
        try input.write(contentsOf: Data("\(value)\n".utf8))
    }

    func finish() throws {
        readState.beginShutdown(by: .now() + shutdownGrace)
        try input.close()
    }

    func cancel() {
        readState.cancel()
        try? input.close()
    }

    deinit {
        readState.cancel()
        try? input.close()
    }

    private nonisolated static func awaitExit(
        _ process: Process, semaphore: DispatchSemaphore, deadline: DispatchTime
    ) -> String? {
        if semaphore.wait(timeout: deadline) == .success { return nil }
        // The bundled supervisor has its own guest deadline. Let it close and
        // reap the worker cooperatively before escalating on this child only.
        let pid = process.processIdentifier
        var forced = false
        if pid > 1 && process.isRunning {
            forced = Darwin.kill(pid, SIGTERM) == 0
        }
        if semaphore.wait(timeout: .now() + .milliseconds(500)) == .timedOut {
            if pid > 1 && process.isRunning && Darwin.kill(pid, SIGKILL) == 0 {
                forced = true
            }
            if semaphore.wait(timeout: .now() + .seconds(1)) == .timedOut {
                return "The session launcher did not exit after shutdown."
            }
        }
        return forced ? "The session launcher had to be terminated after shutdown." : nil
    }

    private nonisolated static func readRecords(
        _ output: FileHandle,
        state: SessionReadState,
        continuation: AsyncStream<AuthoredPESessionEvent>.Continuation
    ) -> String? {
        let descriptor = output.fileDescriptor
        var watched = pollfd(fd: descriptor, events: Int16(POLLIN), revents: 0)
        var transcript = AuthoredPESessionTranscript()
        var buffer = [UInt8](repeating: 0, count: 8192)
        do {
            while !state.isCancelled {
                if let deadline = state.deadline, DispatchTime.now() >= deadline {
                    throw AuthoredPESessionError("The session output did not close before the shutdown deadline.")
                }
                let available = Darwin.poll(&watched, 1, 100)
                if available == 0 && state.didExit {
                    try transcript.end()
                    return nil
                }
                if available == 0 || (available < 0 && errno == EINTR) { continue }
                guard available > 0 else { throw AuthoredPESessionError("Could not monitor session output.") }
                let count = buffer.withUnsafeMutableBytes { Darwin.read(descriptor, $0.baseAddress, $0.count) }
                if count == 0 {
                    try transcript.end()
                    return nil
                }
                if count < 0 && errno == EINTR { continue }
                guard count > 0 else { throw AuthoredPESessionError("Could not read session output.") }
                try buffer.withUnsafeBufferPointer { bytes in
                    try transcript.append(UnsafeBufferPointer(start: bytes.baseAddress, count: count)) { record in
                        continuation.yield(.record(record))
                    }
                }
            }
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    private nonisolated static func collectDiagnostics(_ errors: FileHandle, state: SessionReadState) -> String {
        let limit = 16 * 1024
        var diagnostics = Data()
        var truncated = false
        var buffer = [UInt8](repeating: 0, count: 4096)
        let descriptor = errors.fileDescriptor
        var watched = pollfd(fd: descriptor, events: Int16(POLLIN), revents: 0)
        while !state.shouldStopDiagnostics {
            let available = Darwin.poll(&watched, 1, 100)
            if available == 0 || (available < 0 && errno == EINTR) { continue }
            guard available > 0 else { break }
            let count = buffer.withUnsafeMutableBytes { Darwin.read(descriptor, $0.baseAddress, $0.count) }
            if count < 0 && errno == EINTR { continue }
            guard count > 0 else { break }
            let retained = min(count, limit - diagnostics.count)
            diagnostics.append(contentsOf: buffer.prefix(retained))
            truncated = truncated || retained < count
        }
        let text = String(decoding: diagnostics, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        return truncated ? text + "\nSession diagnostics were truncated." : text
    }
}

@MainActor
final class AuthoredPESessionModel: ObservableObject {
    enum Phase: Equatable {
        case idle, starting, ready, rendering, stopping, finished, failed, cancelled
    }

    @Published private(set) var phase = Phase.idle
    @Published private(set) var frame: AuthoredPESessionFrame?
    @Published private(set) var frames = 0
    @Published private(set) var statusText = ""

    private let executable: URL?
    private let shutdownGrace: TimeInterval
    private var process: AuthoredPESessionProcess?
    private var receivedDone = false
    private var cancelled = false
    private var failure: String?

    init(executable: URL? = AuthoredPESessionModel.workerURL(), shutdownGrace: TimeInterval = 17) {
        self.executable = executable
        self.shutdownGrace = shutdownGrace
    }

    nonisolated static func workerURL() -> URL? {
        if let override = ProcessInfo.processInfo.environment["XENORA_PE_SESSION"], !override.isEmpty {
            return URL(fileURLWithPath: override)
        }
        let bundled = Bundle.main.executableURL?.deletingLastPathComponent()
            .appendingPathComponent("xenora-pe-session")
        guard let bundled, FileManager.default.isExecutableFile(atPath: bundled.path) else { return nil }
        return bundled
    }

    func start(_ configuration: AuthoredPESessionConfiguration) {
        guard process == nil else { return }
        frames = 0
        frame = nil
        receivedDone = false
        cancelled = false
        failure = nil
        guard let executable else {
            setPhase(.failed, "The bundled PE session launcher was not found.")
            return
        }
        do {
            let process = try AuthoredPESessionProcess(
                executable: executable, configuration: configuration, shutdownGrace: shutdownGrace
            )
            self.process = process
            setPhase(.starting, "Starting session…")
            let events = process.events
            Task { [weak self] in
                for await event in events {
                    self?.receive(event)
                }
            }
        } catch {
            setPhase(.failed, error.localizedDescription)
        }
    }

    func send(_ input: UInt32) {
        guard phase == .ready, frames < AuthoredPESessionTranscript.maxFrames, let process else { return }
        setPhase(.rendering, "Waiting for frame \(frames + 1)…")
        do {
            try process.send(input)
        } catch {
            abort("Could not write the session input.")
        }
    }

    func finish() {
        guard phase == .ready, let process else { return }
        setPhase(.stopping, "Waiting for guest cleanup…")
        do {
            try process.finish()
        } catch {
            abort("Could not close the session input.")
        }
    }

    func cancel() {
        guard let process, !cancelled else { return }
        cancelled = true
        setPhase(.stopping, "Cancelling session…")
        process.cancel()
    }

    private func receive(_ event: AuthoredPESessionEvent) {
        switch event {
        case .record(let record):
            guard !cancelled, failure == nil else { return }
            switch record {
            case .ready where phase == .starting:
                setPhase(.ready, "Ready for input.")
            case .frame(let presented) where phase == .rendering:
                frame = presented
                frames = presented.index
                let status = frames == AuthoredPESessionTranscript.maxFrames
                    ? "Frame limit reached. Finish the session to verify guest cleanup."
                    : "Frame \(frames) verified. Ready for input."
                setPhase(.ready, status)
            case .done where phase == .stopping:
                receivedDone = true
            default:
                abort("The session returned a record outside the expected phase.")
            }
        case .exited(let reaped, let status, let problem, let diagnostics):
            process = nil
            if !reaped {
                setPhase(.failed, problem ?? "The session launcher did not exit after shutdown.")
            } else if cancelled {
                setPhase(.cancelled, "Session cancelled.")
            } else if let failure = failure ?? problem {
                setPhase(.failed, failure)
            } else if status == 0 && receivedDone {
                setPhase(.finished, "Session finished with \(frames) verified frames.")
            } else if !diagnostics.isEmpty {
                setPhase(.failed, diagnostics)
            } else if let status, status != 0 {
                setPhase(.failed, "The session launcher exited with status \(status).")
            } else {
                setPhase(.failed, "The session ended without a clean completion.")
            }
        }
    }

    private func abort(_ message: String) {
        failure = message
        setPhase(.stopping, message)
        process?.cancel()
    }

    private func setPhase(_ value: Phase, _ message: String) {
        statusText = message
        phase = value
    }
}
