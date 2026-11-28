// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import Testing
@testable import Xenora

struct ProgramRunnerTests {
    @Test func exportOptionsPrecedeOpaqueGuestArguments() {
        let program = URL(fileURLWithPath: "/temporary files/original guest.exe")
        let destination = URL(fileURLWithPath: "/temporary files/host output.bin")
        let guestPath = #"C:\Guest Files\report.bin"#
        let export = ProgramGuestFileExport(guestPath: guestPath, destination: destination)
        let programArguments = ["--export-guest-file", "two words", ""]

        #expect(ProgramRunner.arguments(
            for: program, trace: true, programArguments: programArguments, guestFileExport: export
        ) == [
            "run", "--experimental", "--trace", "--pe", program.path,
            "--export-guest-file", guestPath, destination.path, "--",
        ] + programArguments)
        #expect(ProgramRunner.arguments(for: program, trace: false, programArguments: []) == [
            "run", "--experimental", "--pe", program.path,
        ])
        #expect(ProgramRunner.arguments(for: program, trace: false, programArguments: ["--startup-report"],
                                        reportStartup: true) == [
            "run", "--experimental", "--pe", program.path, "--startup-report", "--", "--startup-report",
        ])
    }

    @Test @MainActor
    func savePanelRequiresAnUnusedDestination() throws {
        let destination = FileManager.default.temporaryDirectory
            .appendingPathComponent("Xenora export \(UUID().uuidString).bin")
        let policy = GuestFileExportSaveDelegate.shared
        try policy.panel(self, validate: destination)
        try Data([1]).write(to: destination, options: .withoutOverwriting)
        defer { try? FileManager.default.removeItem(at: destination) }
        #expect(throws: CocoaError.self) {
            try policy.panel(self, validate: destination)
        }
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["XENORA_GUEST_FILE_EXPORT_FIXTURE"] != nil))
    func theNativeRunnerExportsTheOriginalGuestBytes() throws {
        let fixturePath = try #require(ProcessInfo.processInfo.environment["XENORA_GUEST_FILE_EXPORT_FIXTURE"])
        let fixture = URL(fileURLWithPath: fixturePath)
        let worker = try #require(ProgramRunner.workerURL())
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("Xenora guest export \(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }

        let destination = directory.appendingPathComponent("host output.bin")
        let export = ProgramGuestFileExport(guestPath: #"C:\guest-export.bin"#, destination: destination)
        let result = ProgramRunner.run(fixture, trace: true, guestFileExport: export, worker: worker)
        #expect(result.launchError == nil)
        #expect(result.exitStatus == 0, "\(result.standardError)")
        #expect(result.guestExitStatus == 0)
        #expect(!result.truncated)
        #expect(try Data(contentsOf: destination) == Data([0, 0x41, 0x0a, 0x0d, 0, 0xff, 0x80]))

        let original = Data([0x31, 0x62, 0x93])
        try original.write(to: destination)
        let collision = ProgramRunner.run(fixture, trace: false, guestFileExport: export, worker: worker)
        #expect(collision.exitStatus == 125, "\(collision.standardError)")
        #expect(collision.guestExitStatus == nil)
        #expect(try Data(contentsOf: destination) == original)

        let unpublished = directory.appendingPathComponent("failed guest.bin")
        let failureExport = ProgramGuestFileExport(guestPath: export.guestPath, destination: unpublished)
        let failure = ProgramRunner.run(fixture, trace: false, programArguments: ["fail"],
                                        guestFileExport: failureExport, worker: worker)
        #expect(failure.exitStatus == 1, "\(failure.standardError)")
        #expect(failure.guestExitStatus == 256)
        #expect(failure.summary == "Exited with status 256.")
        #expect(!failure.standardError.contains("xenora-startup "))
        #expect(!FileManager.default.fileExists(atPath: unpublished.path))
    }

    private static func guestReport(_ guest: UInt64, code: Int32, protocolState: String = "complete") -> String {
        let events = ["parsed", "mapped", "relocated", "imports_bound"]
            .map { #"{"stage":"\#($0)","completed":true}"# }.joined(separator: ",")
        return #"xenora-startup {"schema_version":1,"events":[\#(events)],"last_completed_stage":"imports_bound","protocol":"\#(protocolState)","outcome":{"kind":"guest_exit","guest_status":\#(guest),"worker_code":\#(code)}}"#
    }

    @Test func showsExactGuestStatusRegardlessOfTheHostCode() throws {
        let cases: [(UInt32, Int32)] = [(0, 0), (2, 2), (124, 124), (125, 125), (256, 1), (.max, 255)]
        for (guest, code) in cases {
            let report = Self.guestReport(UInt64(guest), code: code)
            try Self.withWorker(script: """
                test "$5" = --startup-report
                printf '%s' 'unterminated diagnostic' >&2
                printf '\\n%s\\n' '\(report)' >&2
                exit \(code)
                """) { _, worker, program, _ in
                let result = ProgramRunner.run(program, trace: false, worker: worker)
                #expect(result.exitStatus == code)
                #expect(result.guestExitStatus == guest)
                #expect(result.summary == "Exited with status \(guest).")
                #expect(result.standardError == "unterminated diagnostic")
                #expect(!result.truncated)
            }
        }
    }

    @Test func parsesTheFinalReportEvenWhenDisplayedStderrIsBounded() throws {
        let cap = ProgramRunner.outputLimit
        let report = Self.guestReport(256, code: 1)
        for byteCount in [cap - 3, cap + 37] {
            try Self.withWorker(script: """
                printf '%0\(byteCount)d' 0 >&2
                printf '\\n%s\\n' '\(report)' >&2
                exit 1
                """) { _, worker, program, _ in
                let result = ProgramRunner.run(program, trace: false, worker: worker)
                #expect(result.guestExitStatus == 256)
                #expect(result.standardError == String(repeating: "0", count: min(byteCount, cap)))
                #expect(result.truncated == (byteCount > cap))
            }
        }
    }

    @Test func fallsBackUnlessTheFinalReportMatchesTheObservedExit() throws {
        let logs = [
            "plain runner error",
            "xenora-startup {",
            Self.guestReport(125, code: 124),
            Self.guestReport(124, code: 125),
            Self.guestReport(125, code: 125, protocolState: "partial"),
            Self.guestReport(125, code: 125).replacingOccurrences(of: "relocated", with: "skipped_stage"),
            Self.guestReport(UInt64(UInt32.max) + 1, code: 125),
            Self.guestReport(125, code: 125) + "\nmore stderr after the report",
        ]
        for log in logs {
            try Self.withWorker(script: """
                printf '\\n%s\\n' '\(log)' >&2
                exit 125
                """) { _, worker, program, _ in
                let result = ProgramRunner.run(program, trace: false, worker: worker)
                #expect(result.exitStatus == 125)
                #expect(result.guestExitStatus == nil)
                #expect(result.summary == "The run failed (exit 125).")
                #expect(result.standardError == "\n\(log)\n")
            }
        }
    }

    @Test func hidesOnlyTheValidatedFinalReport() throws {
        let earlier = Self.guestReport(125, code: 125)
        let failure = #"xenora-startup {"schema_version":1,"events":[],"last_completed_stage":null,"protocol":"complete","outcome":{"kind":"known_failure","phase":"admission","message":"actual runner error","message_truncated":false,"worker_code":125}}"#
        try Self.withWorker(script: """
            printf '\\n%s\\n' '\(earlier)' >&2
            printf '%s\\n' 'run failed: actual runner error' >&2
            printf '\\n%s\\n' '\(failure)' >&2
            exit 125
            """) { _, worker, program, _ in
            let result = ProgramRunner.run(program, trace: false, worker: worker)
            #expect(result.guestExitStatus == nil)
            #expect(result.summary == "The run failed (exit 125).")
            #expect(result.standardError == "\n\(earlier)\nrun failed: actual runner error\n")
        }
    }

    @Test func environmentOverrideUsesTheLegacyCommandLine() throws {
        let report = Self.guestReport(125, code: 125)
        for trace in [false, true] {
            let optionCheck = trace ? "test \"$3\" = --trace\ntest \"$4\" = --pe" : "test \"$3\" = --pe"
            try Self.withWorker(script: """
                test "$#" = \(trace ? 5 : 4)
                \(optionCheck)
                printf '%s\\n' 'trace stage=exit entry_steps=1' >&2
                printf '\\n%s\\n' '\(report)' >&2
                exit 125
                """) { _, worker, program, _ in
                let result = ProgramRunner.run(program, trace: trace, environment: ["XENORA_CLI": worker.path])
                #expect(result.exitStatus == 125)
                #expect(result.guestExitStatus == nil)
                #expect(result.summary == "The run failed (exit 125) after stage exit.")
                #expect(result.lastStage == "exit")
                #expect(result.standardError == "trace stage=exit entry_steps=1\n\n\(report)\n")
            }
        }
    }

    private static func withWorker(
        script: String? = nil,
        _ body: (_ directory: URL, _ worker: URL, _ program: URL, _ marker: URL) throws -> Void
    ) throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("Xenora stdin \(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }

        let worker = directory.appendingPathComponent("test worker")
        let program = directory.appendingPathComponent("original guest.exe")
        let marker = program.appendingPathExtension("started")
        // The production CLI receives the guest path as argument four when
        // tracing is off. Use it only to mark that a process was launched.
        // The independent system tool reads stdin to EOF and displays raw bytes.
        let bodyScript = script ?? """
            printf 'started' > "${4}.started"
            exec /usr/bin/od -An -v -t x1
            """
        try Data("#!/bin/sh\nset -eu\n\(bodyScript)\n".utf8).write(to: worker)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: worker.path)
        try Data().write(to: program)
        try body(directory, worker, program, marker)
    }

    @Test func theWorkerReceivesExactInputAndEOF() throws {
        try Self.withWorker { directory, worker, program, marker in
            let input = directory.appendingPathComponent("input with spaces.bin")
            let empty = directory.appendingPathComponent("empty.bin")
            try Data([0x00, 0x41, 0x0d, 0x0a, 0xff, 0x80, 0x1a]).write(to: input)
            try Data().write(to: empty)

            let cases: [(URL?, [String])] = [
                (input, ["00", "41", "0d", "0a", "ff", "80", "1a"]),
                (nil, []),
                (empty, []),
            ]
            for (file, expected) in cases {
                let result = ProgramRunner.run(program, trace: false, standardInput: file, worker: worker)
                #expect(result.launchError == nil)
                #expect(result.exitStatus == 0)
                #expect(result.standardError.isEmpty)
                #expect(!result.truncated)
                let actual = result.standardOutput.split(whereSeparator: \.isWhitespace).map(String.init)
                #expect(actual == expected)
                try #require(FileManager.default.fileExists(atPath: marker.path))
                try FileManager.default.removeItem(at: marker)
            }
        }
    }

    @Test func refusedInputNeverLaunchesTheWorker() throws {
        try Self.withWorker { directory, worker, program, marker in
            let missing = directory.appendingPathComponent("missing.bin")
            let large = directory.appendingPathComponent("too large.bin")
            let fifo = directory.appendingPathComponent("input.fifo")
            let fifoLink = directory.appendingPathComponent("input link")
            try Data(repeating: 0x41, count: 1024 * 1024 + 1).write(to: large)
            try #require(Darwin.mkfifo(fifo.path, 0o600) == 0)
            try FileManager.default.createSymbolicLink(at: fifoLink, withDestinationURL: fifo)

            for input in [missing, directory, large, fifo, fifoLink] {
                let result = ProgramRunner.run(program, trace: false, standardInput: input, worker: worker)
                #expect(result.launchError != nil)
                #expect(result.exitStatus == nil)
                #expect(result.standardOutput.isEmpty)
                #expect(result.standardError.isEmpty)
                #expect(!FileManager.default.fileExists(atPath: marker.path))
            }
        }
    }
}
