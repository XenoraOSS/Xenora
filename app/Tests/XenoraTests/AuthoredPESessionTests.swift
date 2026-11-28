// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation
import Testing
@testable import Xenora

@MainActor
struct AuthoredPESessionTests {
    // SHA-256 of ff 00 00 ff, computed independently of the transcript parser.
    private let redDigest = "34aaa746c25a0f105c4316bbb1f009aa359f49582656ee97d73c58132d563423"
    private var redFrame: String { "frame 1 width=1 height=1 sha256=\(redDigest) rgba=ff0000ff" }

    @Test func packageArgumentsPreservePathsAndPins() throws {
        let executable = AuthoredPESessionImage(
            url: URL(fileURLWithPath: "/tmp/root image.exe"), sha256: String(repeating: "AB", count: 32)
        )
        let companion = AuthoredPESessionImage(
            url: URL(fileURLWithPath: "/tmp/extra module.dll"), sha256: String(repeating: "54", count: 32)
        )
        var config = AuthoredPESessionConfiguration(executable: executable, companions: [companion], backend: .metal)
        #expect(try config.arguments() == [
            "--experimental", "--pe", "/tmp/root image.exe", "--sha256", executable.sha256.lowercased(),
            "--graphics-backend", "metal", "--pixels", "--package", "/tmp/extra module.dll",
            companion.sha256, "--deterministic-clock",
        ])
        config.deterministicClock = false
        #expect(try !config.arguments().contains("--deterministic-clock"))

        let badPin = AuthoredPESessionImage(url: executable.url, sha256: String(repeating: "z", count: 64))
        let malformed = AuthoredPESessionConfiguration(executable: executable, companions: [badPin], backend: .reference)
        #expect(throws: AuthoredPESessionError.self) { try malformed.arguments() }
        let oversized = AuthoredPESessionConfiguration(
            executable: executable, companions: Array(repeating: companion, count: 256), backend: .reference
        )
        #expect(throws: AuthoredPESessionError.self) { try oversized.arguments() }
    }

    @Test func transcriptAcceptsSplitAndCoalescedRecords() throws {
        var transcript = AuthoredPESessionTranscript()
        var records: [AuthoredPESessionRecord] = []
        let chunks = ["rea", "dy\n" + redFrame.prefix(31), redFrame.dropFirst(31) + "\ndone frames=1\n"]
        for chunk in chunks {
            try Array(chunk.utf8).withUnsafeBufferPointer { bytes in
                try transcript.append(bytes) { records.append($0) }
            }
        }
        try transcript.end()
        let frame = AuthoredPESessionFrame(index: 1, width: 1, height: 1, rgba: Data([255, 0, 0, 255]), sha256: redDigest)
        #expect(records == [.ready, .frame(frame), .done])
        #expect(frame.image?.width == 1)
    }

    @Test func transcriptRejectsInvalidOutput() {
        let badRecords = [
            redFrame + "\n",
            "ready\nready\n",
            "ready\n" + redFrame.replacingOccurrences(of: "frame 1", with: "frame 2") + "\n",
            "ready\n" + redFrame.replacingOccurrences(of: "width=1", with: "width=257") + "\n",
            "ready\n" + redFrame.replacingOccurrences(of: "rgba=ff0000ff", with: "rgba=ff00") + "\n",
            "ready\n" + redFrame.replacingOccurrences(of: "rgba=ff0000ff", with: "rgba=ff0000fe") + "\n",
            "ready\n" + redFrame.replacingOccurrences(of: "rgba=ff0000ff", with: "rgba=ff0000fZ") + "\n",
            "ready\ndone frames=1\n",
            "ready\ndone frames=0\nready\n",
            "ready\nunfinished",
            "ready\n" + String(repeating: "x", count: 256 * 256 * 8 + 129),
        ]
        for output in badRecords {
            var transcript = AuthoredPESessionTranscript()
            #expect(throws: AuthoredPESessionError.self) {
                try Array(output.utf8).withUnsafeBufferPointer { bytes in
                    try transcript.append(bytes) { _ in }
                }
                try transcript.end()
            }
        }
    }

    @Test func sessionSendsOneInputThenFinishesOnEOF() async throws {
        let worker = try sample("""
            printf 'ready\\n'
            IFS= read -r value
            [ "$value" = "4294967295" ] || exit 3
            printf '%s\\n' '\(redFrame)'
            if IFS= read -r unexpected; then exit 4; fi
            printf 'done frames=1\\n'
            """)
        defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
        let model = AuthoredPESessionModel(executable: worker)
        let first = configuration()
        let next = configuration(path: "/tmp/other/original.exe", digest: String(repeating: "BC", count: 32))
        #expect(model.launchRoot == nil)
        model.start(first)
        try await wait(for: .ready, in: model)
        model.start(next)
        #expect(model.launchRoot == first.executable)
        model.send(UInt32.max)
        model.send(8)
        try await wait(for: .ready, in: model)
        #expect(model.frames == 1)
        #expect(model.frame?.rgba == Data([255, 0, 0, 255]))
        model.finish()
        try await wait(for: .finished, in: model)
        #expect(model.launchRoot == first.executable)
        #expect(model.frame?.sha256 == redDigest)

        model.start(next)
        #expect(model.launchRoot == next.executable)
        #expect(model.frame == nil && model.frames == 0)
        try await wait(for: .ready, in: model)
        model.cancel()
        try await wait(for: .cancelled, in: model)
        #expect(model.launchRoot == next.executable)
    }

    @Test func processStatusAndCompleteTranscriptBothMatter() async throws {
        for script in [
            "printf 'ready\\n'; read -r _",
            "printf 'ready\\n'; read -r _; printf 'done frames=0\\n'; exit 37",
            "printf 'ready\\n'; read -r _; printf 'done frames=0\\ntrailing\\n'",
        ] {
            let worker = try sample(script)
            defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
            let model = AuthoredPESessionModel(executable: worker)
            model.start(configuration())
            try await wait(for: .ready, in: model)
            model.finish()
            try await wait(for: .failed, in: model)
        }
    }

    @Test func cancellationWaitsForExitAndCanRestart() async throws {
        let worker = try sample("""
            printf 'ready\\n'
            read -r _ || exit 0
            """)
        defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
        let model = AuthoredPESessionModel(executable: worker)
        model.start(configuration())
        try await wait(for: .ready, in: model)
        model.cancel()
        #expect(model.phase == .stopping)
        try await wait(for: .cancelled, in: model)

        model.start(configuration())
        try await wait(for: .ready, in: model)
        model.cancel()
        try await wait(for: .cancelled, in: model)
    }

    @Test func cancellationAndInvalidOutputReapAStuckLauncher() async throws {
        for (message, cancel) in [("ready", true), ("invalid", false)] {
            let worker = try sample("""
                trap '' TERM
                printf '%s\\n' '\(message)'
                exec /bin/sleep 8
                """)
            defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
            let model = AuthoredPESessionModel(executable: worker, shutdownGrace: 0.1)
            model.start(configuration())
            if cancel {
                try await wait(for: .ready, in: model)
                model.cancel()
                try await wait(for: .cancelled, in: model)
            } else {
                try await wait(for: .failed, in: model)
                #expect(model.statusText == "The session did not start with a ready record.")
            }
        }
    }

    @Test func doneAndStdoutEOFStillRequireLauncherExit() async throws {
        let worker = try sample("""
            printf 'ready\\n'
            read -r _
            printf 'done frames=0\\n'
            exec 1>&-
            exec /bin/sleep 8
            """)
        defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
        let model = AuthoredPESessionModel(executable: worker, shutdownGrace: 0.1)
        model.start(configuration())
        try await wait(for: .ready, in: model)
        model.finish()
        try await wait(for: .failed, in: model)
        #expect(model.statusText == "The session launcher had to be terminated after shutdown.")
    }

    @Test func inheritedStderrDoesNotDelayCompletion() async throws {
        let worker = try sample("""
            printf 'ready\\n'
            read -r _
            /bin/sleep 3 >&2 &
            printf 'done frames=0\\n'
            """)
        defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
        let model = AuthoredPESessionModel(executable: worker)
        model.start(configuration())
        try await wait(for: .ready, in: model)
        let start = ContinuousClock.now
        model.finish()
        try await wait(for: .finished, in: model)
        #expect(start.duration(to: .now) < .seconds(2))
    }

    @Test func doneWithoutOutputClosureFailsOnShutdownDeadline() async throws {
        let worker = try sample("""
            trap 'exit 0' TERM
            printf 'ready\\n'
            read -r _
            printf 'done frames=0\\n'
            while :; do :; done
            """)
        defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
        let model = AuthoredPESessionModel(executable: worker, shutdownGrace: 0.25)
        model.start(configuration())
        try await wait(for: .ready, in: model)
        model.finish()
        try await wait(for: .failed, in: model)
        #expect(model.statusText == "The session output did not close before the shutdown deadline.")
    }

    @Test func launchAndDiagnosticFailuresAreReported() async throws {
        let first = configuration()
        let missing = AuthoredPESessionModel(executable: URL(fileURLWithPath: "/nonexistent/xenora-pe-session"))
        missing.start(first)
        #expect(missing.phase == .failed)
        #expect(!missing.statusText.isEmpty)
        #expect(missing.launchRoot == first.executable)

        let worker = try sample("""
            printf 'digest rejected\\n' >&2
            printf '%20000s' '' >&2
            exit 125
            """)
        defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
        let model = AuthoredPESessionModel(executable: worker)
        let rejected = configuration(path: "/tmp/rejected.exe")
        model.start(rejected)
        try await wait(for: .failed, in: model)
        #expect(model.statusText.hasPrefix("digest rejected"))
        #expect(model.statusText.hasSuffix("Session diagnostics were truncated."))
        #expect(model.launchRoot == rejected.executable)
        #expect(model.frame == nil)
    }

    @Test func aClosedWorkerInputReportsAWriteFailure() async throws {
        let worker = try sample("""
            exec 0<&-
            printf 'ready\\n'
            sleep 1
            """)
        defer { try? FileManager.default.removeItem(at: worker.deletingLastPathComponent()) }
        let model = AuthoredPESessionModel(executable: worker)
        model.start(configuration())
        try await wait(for: .ready, in: model)
        model.send(10)
        try await wait(for: .failed, in: model)
        #expect(model.statusText == "Could not write the session input.")
    }

    private func configuration(path: String = "/tmp/original.exe",
                               digest: String = String(repeating: "a", count: 64)) -> AuthoredPESessionConfiguration {
        AuthoredPESessionConfiguration(
            executable: AuthoredPESessionImage(url: URL(fileURLWithPath: path), sha256: digest),
            companions: [],
            backend: .reference
        )
    }

    private func sample(_ script: String) throws -> URL {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        let url = directory.appendingPathComponent("worker")
        try ("#!/bin/sh\n" + script + "\n").write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: url.path)
        return url
    }

    private func wait(for phase: AuthoredPESessionModel.Phase, in model: AuthoredPESessionModel) async throws {
        let deadline = ContinuousClock.now + .seconds(4)
        while model.phase != phase, ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(model.phase == phase, "\(model.phase): \(model.statusText)")
    }
}
