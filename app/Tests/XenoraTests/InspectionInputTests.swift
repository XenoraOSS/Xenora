// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation
import Testing
@testable import Xenora

struct InspectionInputTests {
    private func withDirectory(_ body: (URL) throws -> Void) throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("Xenora inspect \(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        defer { try? FileManager.default.removeItem(at: directory) }
        try body(directory)
    }

    @Test func readsTheExactLimitAcrossChunksAndThroughASymlink() throws {
        try withDirectory { directory in
            let file = directory.appendingPathComponent("input file")
            let link = directory.appendingPathComponent("regular link")
            let bytes = Data((0..<65_543).map { UInt8(truncatingIfNeeded: $0) })
            try bytes.write(to: file)
            try FileManager.default.createSymbolicLink(at: link, withDestinationURL: file)

            #expect(try InspectionInput.read(file, limit: bytes.count) == bytes)
            #expect(try InspectionInput.read(link, limit: bytes.count) == bytes)
            #expect(throws: InspectionInputError.tooLarge) {
                try InspectionInput.read(file, limit: bytes.count - 1)
            }
        }
    }

    @Test func refusesAnAdditionalByteWhileReadingFromTheOpenDescriptor() throws {
        try withDirectory { directory in
            let file = directory.appendingPathComponent("input")
            try Data([0x21, 0x43, 0x65, 0x87]).write(to: file)
            let descriptor = Darwin.open(file.path, O_RDONLY | O_CLOEXEC)
            try #require(descriptor >= 0)
            defer { _ = Darwin.close(descriptor) }

            #expect(throws: InspectionInputError.tooLarge) {
                try InspectionInput.readContents(from: descriptor, limit: 2)
            }
            #expect(Darwin.lseek(descriptor, 0, SEEK_CUR) == 3)
        }
    }

    @Test func refusesNonregularAndMissingPaths() throws {
        try withDirectory { directory in
            let fifo = directory.appendingPathComponent("pipe")
            let link = directory.appendingPathComponent("pipe link")
            let missing = directory.appendingPathComponent("missing")
            try #require(Darwin.mkfifo(fifo.path, 0o600) == 0)
            try FileManager.default.createSymbolicLink(at: link, withDestinationURL: fifo)

            for path in [directory, fifo, link] {
                #expect(throws: InspectionInputError.notRegular) {
                    try InspectionInput.read(path, limit: 16)
                }
            }
            #expect(throws: InspectionInputError.cannotRead(ENOENT)) {
                try InspectionInput.read(missing, limit: 16)
            }
        }
    }
}
