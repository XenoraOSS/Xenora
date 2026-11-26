// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import Testing
@testable import Xenora

struct ProgramRunnerTests {
    private static func withWorker(
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
        let script = """
            #!/bin/sh
            set -eu
            printf 'started' > "${4}.started"
            exec /usr/bin/od -An -v -t x1

            """
        try Data(script.utf8).write(to: worker)
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
