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

import Foundation
import Metal
import SwiftUI

/// What one run of the worker produced.
struct ProgramRunResult: Equatable {
    /// The worker's exit status: the program's own, 124 for the wall
    /// deadline, 125 for any other failure of the runner or the guest, 2 for
    /// a refused command line. `nil` if the worker could not be started or
    /// ended by a signal.
    let exitStatus: Int32?
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

    /// The bundled worker, or the one named by `XENORA_CLI` (for `swift run`).
    static func workerURL() -> URL? {
        if let override = ProcessInfo.processInfo.environment["XENORA_CLI"], !override.isEmpty {
            return URL(fileURLWithPath: override)
        }
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

    /// The worker's command line for one run. Tracing is on when the HUD is,
    /// because a program run has no Metal HUD in the runner yet (#440) and
    /// its startup stages are the statistics it has.
    static func arguments(for file: URL, trace: Bool, programArguments: [String]) -> [String] {
        var arguments = ["run", "--experimental"]
        if trace { arguments.append("--trace") }
        arguments += ["--pe", file.path]
        if !programArguments.isEmpty {
            arguments.append("--")
            arguments += programArguments
        }
        return arguments
    }

    /// The last `trace stage=<name>` line's stage name.
    static func lastStage(in text: String) -> String? {
        text.split(separator: "\n").reversed().lazy.compactMap { line -> String? in
            guard line.hasPrefix("trace stage=") else { return nil }
            return line.dropFirst("trace stage=".count).split(separator: " ").first.map(String.init)
        }.first
    }

    /// Run the worker and wait for it. Blocking: call it off the main thread.
    static func run(_ file: URL, trace: Bool, programArguments: [String] = [],
                    worker: URL? = workerURL()) -> ProgramRunResult {
        let started = Date()
        func failed(_ reason: String) -> ProgramRunResult {
            ProgramRunResult(exitStatus: nil, standardOutput: "", standardError: "", truncated: false,
                             lastStage: nil, launchError: reason,
                             hostMilliseconds: Date().timeIntervalSince(started) * 1000)
        }
        guard let worker else {
            return failed("the bundled xenora runner was not found next to the app")
        }
        let scoped = file.startAccessingSecurityScopedResource()
        defer { if scoped { file.stopAccessingSecurityScopedResource() } }

        let process = Process()
        process.executableURL = worker
        process.arguments = arguments(for: file, trace: trace, programArguments: programArguments)
        process.standardInput = FileHandle.nullDevice
        let out = Pipe()
        let err = Pipe()
        process.standardOutput = out
        process.standardError = err
        let collectors = [BoundedCollector(out.fileHandleForReading),
                          BoundedCollector(err.fileHandleForReading)]
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
        let (stdout, stdoutCut) = collectors[0].finish()
        let (stderr, stderrCut) = collectors[1].finish()
        let status: Int32? = process.terminationReason == .exit ? process.terminationStatus : nil
        return ProgramRunResult(
            exitStatus: status, standardOutput: stdout, standardError: stderr,
            truncated: stdoutCut || stderrCut, lastStage: lastStage(in: stderr), launchError: nil,
            hostMilliseconds: Date().timeIntervalSince(started) * 1000)
    }
}

/// Reads a pipe to its end on a background queue, keeping at most
/// `ProgramRunner.outputLimit` bytes so a chatty program cannot exhaust memory
/// or block on a full pipe.
private final class BoundedCollector: @unchecked Sendable {
    private var data = Data()
    private var cut = false
    private let lock = NSLock()
    private let finished = DispatchSemaphore(value: 0)

    init(_ handle: FileHandle) {
        DispatchQueue.global().async { [self] in
            while true {
                let chunk = handle.availableData
                if chunk.isEmpty { break }
                lock.lock()
                let room = ProgramRunner.outputLimit - data.count
                if chunk.count > room { cut = true }
                if room > 0 { data.append(chunk.prefix(room)) }
                lock.unlock()
            }
            finished.signal()
        }
    }

    func finish() -> (String, Bool) {
        finished.wait()
        lock.lock()
        defer { lock.unlock() }
        return (String(decoding: data, as: UTF8.self), cut)
    }
}

/// The latest run of a chosen executable. Only the latest request publishes.
@MainActor
final class ProgramRunModel: ObservableObject {
    static let shared = ProgramRunModel()
    @Published var running = false
    @Published var result: ProgramRunResult?
    @Published var fileName: String?
    @Published var argumentText = ""
    private var latestRequest: UInt64 = 0

    func run(_ url: URL, trace: Bool) {
        latestRequest &+= 1
        let request = latestRequest
        fileName = url.lastPathComponent
        result = nil
        running = true
        let arguments = argumentText.split(whereSeparator: \.isWhitespace).map(String.init)
        Task {
            let outcome = await Task.detached {
                ProgramRunner.run(url, trace: trace, programArguments: arguments)
            }.value
            guard request == latestRequest else { return }
            result = outcome
            running = false
        }
    }

    /// A different file was chosen: its run starts empty.
    func clear() {
        latestRequest &+= 1
        running = false
        result = nil
        fileName = nil
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
    let url: URL?
    @ObservedObject var model: ProgramRunModel
    @ObservedObject var hud: HudSettings

    var body: some View {
        Section("Run executable (experimental)") {
            HStack {
                TextField("Program arguments", text: $model.argumentText)
                    .textFieldStyle(.roundedBorder)
                Button(model.running ? "Running…" : "Run") {
                    if let url { model.run(url, trace: hud.enabled) }
                }
                .disabled(url == nil || model.running)
                .keyboardShortcut("r")
            }
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
