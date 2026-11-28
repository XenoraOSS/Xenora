// SPDX-License-Identifier: GPL-3.0-or-later
//
// Running a chosen executable, and the Metal HUD switch.
//
// A run goes through the same `xenora run` worker as the command line (#439),
// which `scripts/build-app.sh` bundles next to the app's executable as
// `xenora-cli`. The app
// adds no execution path of its own: the worker's separate process, wall
// deadline and step limit apply unchanged. Running a file is not a claim that
// it is supported; most executables stop at an unresolved import or an
// unsupported instruction, and the run reports where.

import Darwin
import AppKit
import Foundation
import Metal
import SwiftUI
import UniformTypeIdentifiers

struct ProgramGuestFileExport: Equatable, Sendable {
    let guestPath: String
    let destination: URL

    static func validationError(guestPath: String, destination: URL?) -> String? {
        let prefix = guestPath.prefix(3).uppercased()
        guard (prefix == "C:\\" || prefix == "C:/"), guestPath.count > 3,
              !guestPath.utf8.contains(0), !guestPath.hasSuffix("\\"), !guestPath.hasSuffix("/") else {
            return "Enter an absolute guest file path on the C: drive."
        }
        guard let destination, destination.isFileURL, !destination.path.utf8.contains(0) else {
            return "Choose a local export destination."
        }
        return nil
    }
}

@MainActor
final class GuestFileExportSaveDelegate: NSObject, NSOpenSavePanelDelegate {
    static let shared = GuestFileExportSaveDelegate()

    func panel(_ sender: Any, validate url: URL) throws {
        if (try? FileManager.default.attributesOfItem(atPath: url.path)) != nil {
            throw CocoaError(.fileWriteFileExists, userInfo: [
                NSLocalizedDescriptionKey: "Choose a new filename for the exported file."
            ])
        }
    }
}

/// What one run of the worker produced.
struct ProgramRunResult: Equatable, Sendable {
    /// The POSIX exit code of the launcher, if it exited normally.
    let exitStatus: Int32?
    /// The full guest exit status from a matching supervisor report.
    let guestExitStatus: UInt32?
    let standardOutput: String
    let standardError: String
    /// Whether either stream was cut at `ProgramRunner.outputLimit`.
    let truncated: Bool
    /// The last startup stage the worker traced, if tracing was on.
    let lastStage: String?
    /// Why the worker could not run, when it never started.
    let launchError: String?
    let hostMilliseconds: Double

    var summary: String {
        if let launchError { return "Could not start the runner: \(launchError)" }
        if let guestExitStatus { return "Exited with status \(guestExitStatus)." }
        guard let status = exitStatus else { return "The runner ended without an exit status." }
        switch status {
        case 124: return "Stopped at the wall deadline (exit 124)."
        case 125: return "The run failed (exit 125)\(lastStage.map { " after stage \($0)" } ?? "")."
        case 2: return "The runner refused the command line (exit 2)."
        default: return "Exited with status \(status)."
        }
    }
}

enum ProgramRunner {
    /// Bytes kept of each output stream; the rest is dropped and flagged.
    static let outputLimit = 256 * 1024
    /// A backstop only: the worker enforces its own 15-second wall deadline.
    static let backstopSeconds: Double = 60
    /// Keep the file admission limit in sync with the CLI worker's input limit.
    private static let standardInputLimit = 1 << 20

    private enum InputFileError: LocalizedError {
        case notRegular
        case tooLarge
        case posix(Int32)

        var errorDescription: String? {
            switch self {
            case .notRegular: return "Expected a regular file."
            case .tooLarge: return "The file exceeds the 1 MiB standard input limit."
            case let .posix(code):
                return NSError(domain: NSPOSIXErrorDomain, code: Int(code)).localizedDescription
            }
        }
    }

    private static func openStandardInput(_ url: URL) throws -> FileHandle {
        try url.withUnsafeFileSystemRepresentation { path in
            guard url.isFileURL, let path else { throw InputFileError.notRegular }
            var info = stat()
            guard Darwin.fstatat(AT_FDCWD, path, &info, 0) == 0 else {
                throw InputFileError.posix(errno)
            }
            guard info.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG) else {
                throw InputFileError.notRegular
            }

            // The path may have been replaced after stat. A FIFO must never
            // block the app, and the opened descriptor decides what to admit.
            let descriptor = Darwin.open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC)
            guard descriptor >= 0 else { throw InputFileError.posix(errno) }
            let handle = FileHandle(fileDescriptor: descriptor, closeOnDealloc: true)
            guard Darwin.fstat(descriptor, &info) == 0 else {
                throw InputFileError.posix(errno)
            }
            guard info.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG) else {
                throw InputFileError.notRegular
            }
            guard info.st_size <= off_t(standardInputLimit) else { throw InputFileError.tooLarge }
            return handle
        }
    }

    /// The bundled worker, or the one named by `XENORA_CLI` (for `swift run`).
    static func workerURL() -> URL? {
        workerOverride(in: ProcessInfo.processInfo.environment) ?? bundledWorkerURL()
    }

    private static func workerOverride(in environment: [String: String]) -> URL? {
        guard let override = environment["XENORA_CLI"], !override.isEmpty else { return nil }
        return URL(fileURLWithPath: override)
    }

    private static func bundledWorkerURL() -> URL? {
        // Not "xenora": the default macOS file system ignores case, so that
        // name would be the app's own "Xenora" executable.
        let bundled = Bundle.main.executableURL?
            .deletingLastPathComponent()
            .appendingPathComponent("xenora-cli")
        guard let bundled, FileManager.default.isExecutableFile(atPath: bundled.path) else {
            return nil
        }
        return bundled
    }

    /// Header-only inspection uses the same bounded file reader as the CLI.
    static func inspectHeaders(_ file: URL) -> (Bool, String) {
        guard let worker = workerURL() else { return (false, "The bundled xenora runner was not found.") }
        let process = Process()
        process.executableURL = worker
        process.arguments = ["inspect", file.path]
        process.standardInput = FileHandle.nullDevice
        let out = Pipe()
        let err = Pipe()
        process.standardOutput = out
        process.standardError = err
        let stdout = BoundedCollector(out.fileHandleForReading)
        let stderr = BoundedCollector(err.fileHandleForReading)
        do {
            try process.run()
        } catch {
            return (false, "Could not start the inspector: \(error.localizedDescription)")
        }
        let done = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in done.signal() }
        if done.wait(timeout: .now() + backstopSeconds) == .timedOut {
            process.terminate()
            if done.wait(timeout: .now() + 2) == .timedOut {
                _ = Darwin.kill(process.processIdentifier, SIGKILL)
                process.waitUntilExit()
            }
            _ = stdout.finish()
            _ = stderr.finish()
            return (false, "Header inspection exceeded its deadline.")
        }
        let output = stdout.finish()
        let failure = stderr.finish()
        let truncated = output.truncated || failure.truncated ? "\nInspection output was truncated.\n" : ""
        let ok = process.terminationReason == .exit && process.terminationStatus == 0
        return (ok, "Header-only inspection; detailed import and instruction analysis was not run.\n"
                + output.text + failure.text + truncated)
    }

    /// The worker's command line for one run. Tracing is on when the HUD is,
    /// because a program run has no Metal HUD in the runner yet (#440) and
    /// its startup stages are the statistics it has.
    static func arguments(for file: URL, trace: Bool, programArguments: [String],
                          guestFileExport: ProgramGuestFileExport? = nil,
                          reportStartup: Bool = false) -> [String] {
        var arguments = ["run", "--experimental"]
        if trace { arguments.append("--trace") }
        arguments += ["--pe", file.path]
        if reportStartup { arguments.append("--startup-report") }
        if let guestFileExport {
            arguments += ["--export-guest-file", guestFileExport.guestPath, guestFileExport.destination.path]
        }
        if !programArguments.isEmpty {
            arguments.append("--")
            arguments += programArguments
        }
        return arguments
    }

    /// The last `trace stage=<name>` line's stage name.
    static func lastStage(in text: String) -> String? {
        text.split(whereSeparator: \.isNewline).reversed().lazy.compactMap { line -> String? in
            guard line.hasPrefix("trace stage=") else { return nil }
            return line.dropFirst("trace stage=".count).split(separator: " ").first.map(String.init)
        }.first
    }

    /// Run the worker and wait for it. Blocking: call it off the main thread.
    static func run(_ file: URL, trace: Bool, programArguments: [String] = [],
                    standardInput: URL? = nil,
                    guestFileExport: ProgramGuestFileExport? = nil,
                    environment: [String: String] = ProcessInfo.processInfo.environment) -> ProgramRunResult {
        let override = workerOverride(in: environment)
        return run(file, trace: trace, programArguments: programArguments, standardInput: standardInput,
                   guestFileExport: guestFileExport, worker: override ?? bundledWorkerURL(),
                   reportStartup: override == nil)
    }

    /// Run an explicitly selected worker. Tests can supply a report-capable runner.
    static func run(_ file: URL, trace: Bool, programArguments: [String] = [],
                    standardInput: URL? = nil, guestFileExport: ProgramGuestFileExport? = nil,
                    worker: URL?, reportStartup: Bool = true) -> ProgramRunResult {
        let started = Date()
        func failed(_ reason: String) -> ProgramRunResult {
            ProgramRunResult(exitStatus: nil, guestExitStatus: nil, standardOutput: "", standardError: "", truncated: false,
                             lastStage: nil, launchError: reason,
                             hostMilliseconds: Date().timeIntervalSince(started) * 1000)
        }
        guard let worker else {
            return failed("the bundled xenora runner was not found next to the app")
        }
        if let guestFileExport, let error = ProgramGuestFileExport.validationError(
            guestPath: guestFileExport.guestPath, destination: guestFileExport.destination
        ) {
            return failed(error)
        }
        let scoped = file.startAccessingSecurityScopedResource()
        defer { if scoped { file.stopAccessingSecurityScopedResource() } }
        let inputScoped = standardInput?.startAccessingSecurityScopedResource() ?? false
        defer { if inputScoped { standardInput?.stopAccessingSecurityScopedResource() } }
        let destination = guestFileExport?.destination
        let destinationScoped = destination?.startAccessingSecurityScopedResource() ?? false
        defer { if destinationScoped { destination?.stopAccessingSecurityScopedResource() } }
        let input: FileHandle?
        if let standardInput {
            do {
                input = try openStandardInput(standardInput)
            } catch {
                return failed("Could not use standard input file \(standardInput.lastPathComponent): \(error.localizedDescription)")
            }
        } else {
            input = nil
        }
        defer { try? input?.close() }

        let process = Process()
        process.executableURL = worker
        process.arguments = arguments(for: file, trace: trace, programArguments: programArguments,
                                      guestFileExport: guestFileExport, reportStartup: reportStartup)
        process.standardInput = input ?? FileHandle.nullDevice
        let out = Pipe()
        let err = Pipe()
        process.standardOutput = out
        process.standardError = err
        let output = BoundedCollector(out.fileHandleForReading)
        let diagnostics = BoundedCollector(err.fileHandleForReading, tailLimit: reportStartup ? StartupReport.tailLimit : 0)
        do {
            try process.run()
        } catch {
            return failed(error.localizedDescription)
        }
        let done = DispatchSemaphore(value: 0)
        process.terminationHandler = { _ in done.signal() }
        if done.wait(timeout: .now() + backstopSeconds) == .timedOut {
            process.terminate()
            process.waitUntilExit()
        }
        let stdout = output.finish()
        var stderr = diagnostics.finish()
        let status: Int32? = process.terminationReason == .exit ? process.terminationStatus : nil
        let guestStatus = StartupReport.consumeFinalRecord(from: &stderr, exitStatus: status)
        return ProgramRunResult(
            exitStatus: status, guestExitStatus: guestStatus, standardOutput: stdout.text, standardError: stderr.text,
            truncated: stdout.truncated || stderr.truncated,
            lastStage: trace ? lastStage(in: stderr.text) : nil, launchError: nil,
            hostMilliseconds: Date().timeIntervalSince(started) * 1000)
    }
}

private struct StartupReport: Decodable {
    // The runner bounds the diagnostic socket to 2 KiB before JSON encoding.
    // Allow room for JSON escaping as well as the fixed report fields.
    static let tailLimit = 16 * 1024
    private static let prefix = Array("xenora-startup ".utf8)
    private static let stages = [
        "parsed", "mapped", "relocated", "imports_bound", "tls_completed",
        "entry_started", "entry_returned", "framebuffer_verified",
    ]

    struct Event: Decodable {
        let stage: String
        let completed: Bool
    }

    struct Outcome: Decodable {
        let kind: String
        let guestStatus: UInt32?
        let workerCode: Int32?
        let workerSignal: Int32?
        let phase: String?
        let message: String?
        let messageTruncated: Bool?
    }

    let schemaVersion: Int
    let events: [Event]
    let lastCompletedStage: String?
    let `protocol`: String
    let outcome: Outcome

    static func consumeFinalRecord(from output: inout CapturedOutput, exitStatus: Int32?) -> UInt32? {
        let tail = output.tail
        // The supervisor appends a leading LF and one final LF. Require both
        // so earlier messages and incomplete or overlong reports stay visible.
        guard tail.last == 10, let separator = tail.dropLast().lastIndex(of: 10) else { return nil }
        let line = tail.dropLast().suffix(from: tail.index(after: separator))
        guard line.starts(with: prefix) else { return nil }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let report = try? decoder.decode(Self.self, from: Data(line.dropFirst(prefix.count))),
              let exitStatus, report.matches(exitStatus: exitStatus) else { return nil }
        output.removeSuffix(byteCount: tail.distance(from: separator, to: tail.endIndex))
        return report.outcome.guestStatus
    }

    private func matches(exitStatus: Int32) -> Bool {
        guard schemaVersion == 1, self.protocol == "complete", outcome.workerSignal == nil,
              outcome.workerCode == exitStatus,
              events.map(\.stage) == Array(Self.stages.prefix(events.count)),
              events.allSatisfy({ $0.completed == ($0.stage != "entry_started") }),
              events.last(where: \.completed)?.stage == lastCompletedStage else { return false }
        switch outcome.kind {
        case "guest_exit":
            guard let guest = outcome.guestStatus,
                  let last = events.last?.stage,
                  ["imports_bound", "tls_completed", "entry_returned"].contains(last) else { return false }
            let lowByte = Int32(guest & 0xff)
            return exitStatus == (guest != 0 && lowByte == 0 ? 1 : lowByte)
        case "verified":
            return outcome.guestStatus == nil && exitStatus == 0 && lastCompletedStage == "framebuffer_verified"
        case "wall_timeout": return outcome.guestStatus == nil && exitStatus == 124
        case "known_failure":
            return outcome.guestStatus == nil && exitStatus == 125
                && outcome.phase != nil && outcome.message != nil && outcome.messageTruncated != nil
        default: return false
        }
    }
}

private struct CapturedOutput {
    var prefix = Data()
    var tail = Data()
    var byteCount = 0

    var text: String { String(decoding: prefix, as: UTF8.self) }
    var truncated: Bool { byteCount > prefix.count }

    mutating func removeSuffix(byteCount length: Int) {
        byteCount -= length
        if prefix.count > byteCount { prefix.removeLast(prefix.count - byteCount) }
    }
}

/// Drain the pipe while retaining a bounded display prefix and optional tail.
private final class BoundedCollector: @unchecked Sendable {
    private var output = CapturedOutput()
    private let lock = NSLock()
    private let finished = DispatchSemaphore(value: 0)

    init(_ handle: FileHandle, tailLimit: Int = 0) {
        DispatchQueue.global().async { [self] in
            while true {
                let chunk = handle.availableData
                if chunk.isEmpty { break }
                lock.lock()
                output.byteCount += chunk.count
                let room = ProgramRunner.outputLimit - output.prefix.count
                if room > 0 { output.prefix.append(chunk.prefix(room)) }
                if tailLimit > 0 {
                    let suffix = chunk.suffix(tailLimit)
                    let excess = max(0, output.tail.count + suffix.count - tailLimit)
                    output.tail.removeFirst(excess)
                    output.tail.append(suffix)
                }
                lock.unlock()
            }
            finished.signal()
        }
    }

    func finish() -> CapturedOutput {
        finished.wait()
        lock.lock()
        defer { lock.unlock() }
        return output
    }
}

struct ProgramArgument: Identifiable {
    let id = UUID()
    var value = ""
}

/// Owns one worker slot and publishes results for the current selection.
@MainActor
final class ProgramRunModel: ObservableObject {
    typealias Worker = @Sendable (URL, Bool, [String], URL?, ProgramGuestFileExport?) async -> ProgramRunResult

    static let shared = ProgramRunModel()
    @Published private(set) var running = false
    @Published private(set) var result: ProgramRunResult?
    @Published private(set) var fileName: String?
    @Published var arguments: [ProgramArgument] = []
    @Published var standardInput: URL?
    @Published var guestFilePath = ""
    @Published var guestFileDestination: URL?
    private let worker: Worker
    private(set) var latestRequest: UInt64 = 0

    init(worker: @escaping Worker = { url, trace, arguments, standardInput, guestFileExport in
        await Task.detached {
            ProgramRunner.run(url, trace: trace, programArguments: arguments, standardInput: standardInput,
                              guestFileExport: guestFileExport)
        }.value
    }) {
        self.worker = worker
    }

    var workerArguments: [String] { arguments.map(\.value) }

    var guestFileExportError: String? {
        guard !guestFilePath.isEmpty || guestFileDestination != nil else { return nil }
        return ProgramGuestFileExport.validationError(guestPath: guestFilePath, destination: guestFileDestination)
    }

    func clearGuestFileExport() {
        guestFilePath = ""
        guestFileDestination = nil
    }

    func addArgument() -> UUID {
        let argument = ProgramArgument()
        arguments.append(argument)
        return argument.id
    }

    /// Returns the neighboring row that should receive focus after removal.
    func removeArgument(_ id: UUID) -> UUID? {
        guard let index = arguments.firstIndex(where: { $0.id == id }) else { return nil }
        arguments.remove(at: index)
        return index < arguments.count ? arguments[index].id : arguments.last?.id
    }

    @discardableResult
    func run(_ url: URL, trace: Bool) -> Task<Void, Never>? {
        guard !running, guestFileExportError == nil else { return nil }
        latestRequest &+= 1
        let request = latestRequest
        fileName = url.lastPathComponent
        result = nil
        running = true
        let arguments = workerArguments
        let standardInput = self.standardInput
        let guestFileExport = guestFileDestination.map {
            ProgramGuestFileExport(guestPath: guestFilePath, destination: $0)
        }
        return Task {
            let outcome = await worker(url, trace, arguments, standardInput, guestFileExport)
            if request == latestRequest {
                result = outcome
            }
            running = false
        }
    }

    /// Clear the old selection's output while its worker finishes.
    func clear() {
        latestRequest &+= 1
        result = nil
        fileName = nil
        standardInput = nil
        clearGuestFileExport()
    }
}

/// Whether the Metal HUD is shown. Off by default; remembered between launches.
@MainActor
final class HudSettings: ObservableObject {
    static let key = "MetalHUDEnabled"
    static let shared = HudSettings()
    private let defaults: UserDefaults

    @Published var enabled: Bool {
        didSet { defaults.set(enabled, forKey: Self.key) }
    }

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        enabled = defaults.bool(forKey: Self.key)
    }

    /// The Metal device frames are rendered on, for the HUD's first field.
    nonisolated static let deviceName: String = MTLCreateSystemDefaultDevice()?.name ?? "no Metal device"
}

/// One HUD overlay. Every number is measured on the host by the app, except
/// the frame counters, which come from the runtime.
struct MetalHudOverlay: View {
    let lines: [String]

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text("Metal HUD · \(HudSettings.deviceName)").bold()
            ForEach(lines, id: \.self) { Text($0) }
        }
        .font(.system(.caption2, design: .monospaced))
        .padding(6)
        .background(.black.opacity(0.7), in: RoundedRectangle(cornerRadius: 6))
        .foregroundStyle(.white)
        .padding(6)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Metal HUD")
    }
}

extension View {
    /// Overlays the HUD in the top leading corner when it is enabled.
    func metalHud(_ enabled: Bool, _ lines: [String]) -> some View {
        overlay(alignment: .topLeading) {
            if enabled { MetalHudOverlay(lines: lines) }
        }
    }
}

/// Run controls and results for the inspected file.
struct ProgramRunView: View {
    private enum ArgumentFocus: Hashable {
        case row(UUID), add
    }

    let url: URL?
    @ObservedObject var model: ProgramRunModel
    @ObservedObject var hud: HudSettings
    @FocusState private var argumentFocus: ArgumentFocus?
    @State private var standardInputImportError: String?

    private var runButtonTitle: String {
        guard model.running else { return "Run" }
        return model.fileName == nil ? "Finishing previous run…" : "Running…"
    }

    var body: some View {
        Section("Run executable (experimental)") {
            argumentEditor
            standardInputEditor
            guestFileExportEditor
            Button(runButtonTitle) {
                if let url { model.run(url, trace: hud.enabled) }
            }
            .disabled(url == nil || model.running || model.guestFileExportError != nil)
            .keyboardShortcut("r")
            if url == nil {
                Text("Open an executable above to run it.").foregroundStyle(.secondary)
            }
            if let result = model.result, let name = model.fileName {
                LabeledContent("Result", value: "\(name): \(result.summary)")
                    .foregroundStyle(result.exitStatus == 0 ? Color.primary : Color.red)
                if hud.enabled {
                    MetalHudOverlay(lines: [
                        String(format: "host wall %.1f ms", result.hostMilliseconds),
                        "last stage \(result.lastStage ?? "none traced")",
                        "no Metal frame: program runs have no draw path yet",
                    ])
                }
                output("Standard output", result.standardOutput)
                output("Standard error", result.standardError)
                if result.truncated {
                    Text("Output was cut at \(ProgramRunner.outputLimit / 1024) KiB per stream.")
                        .font(.caption).foregroundStyle(.secondary)
                }
            }
            Text("Runs go through the bundled xenora runner with its own process, 15-second deadline and step limit. Most executables stop at an unresolved import or unsupported instruction; running one is not a claim that it is supported.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    private var standardInputEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            LabeledContent("Standard input", value: model.standardInput?.lastPathComponent ?? "No input file")
                .accessibilityIdentifier("program-stdin-file")
            HStack {
                Button("Choose standard input file…") { chooseStandardInput() }
                    .accessibilityIdentifier("program-stdin-choose")
                if model.standardInput != nil {
                    Button("Clear input file") {
                        model.standardInput = nil
                        standardInputImportError = nil
                    }
                    .accessibilityIdentifier("program-stdin-clear")
                }
            }
            .disabled(model.running)
            if let standardInputImportError {
                Text(standardInputImportError).foregroundStyle(.red)
            }
        }
        .onChange(of: url) { _, _ in standardInputImportError = nil }
    }

    private func chooseStandardInput() {
        let panel = NSOpenPanel()
        panel.title = "Choose Standard Input File"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.begin { response in
            guard response == .OK, let file = panel.url else { return }
            model.standardInput = file
            standardInputImportError = nil
        }
    }

    private var guestFileExportEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            TextField("Guest file to export (optional)", text: $model.guestFilePath,
                      prompt: Text("C:\\output.bin"))
                .textFieldStyle(.roundedBorder)
                .accessibilityIdentifier("program-export-guest-path")
            LabeledContent("Export destination", value: model.guestFileDestination?.lastPathComponent ?? "No destination")
                .accessibilityIdentifier("program-export-destination")
            HStack {
                Button("Choose export destination…") { chooseGuestFileDestination() }
                    .accessibilityIdentifier("program-export-choose")
                if !model.guestFilePath.isEmpty || model.guestFileDestination != nil {
                    Button("Clear export") { model.clearGuestFileExport() }
                        .accessibilityIdentifier("program-export-clear")
                }
            }
            if let error = model.guestFileExportError {
                Text(error).font(.caption).foregroundStyle(.secondary)
            }
        }
        .disabled(url == nil || model.running)
    }

    private func chooseGuestFileDestination() {
        let request = model.latestRequest
        let panel = NSSavePanel()
        panel.title = "Choose Guest File Export Destination"
        panel.message = "Choose a new filename for the exported file."
        panel.delegate = GuestFileExportSaveDelegate.shared
        panel.begin { response in
            guard response == .OK, let destination = panel.url,
                  request == model.latestRequest else { return }
            model.guestFileDestination = destination
        }
    }

    private var argumentEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Program arguments").font(.headline)
            ForEach($model.arguments) { $argument in
                let position = model.arguments.firstIndex(where: { $0.id == argument.id }) ?? 0
                HStack {
                    TextField("Argument \(position + 1)", text: $argument.value)
                        .textFieldStyle(.roundedBorder)
                        .focused($argumentFocus, equals: .row(argument.id))
                    Button("Remove") {
                        argumentFocus = model.removeArgument(argument.id).map(ArgumentFocus.row) ?? .add
                    }
                    .accessibilityLabel("Remove argument \(position + 1)")
                }
            }
            Button("Add argument") {
                argumentFocus = .row(model.addArgument())
            }
            .focused($argumentFocus, equals: .add)
            Text("One row per argument. A blank row passes an empty argument.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private func output(_ title: String, _ text: String) -> some View {
        if !text.isEmpty {
            Text(title).font(.caption).foregroundStyle(.secondary)
            ScrollView {
                Text(text)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 60, maxHeight: 200)
        }
    }
}
