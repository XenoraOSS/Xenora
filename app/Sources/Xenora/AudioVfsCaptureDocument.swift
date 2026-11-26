// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import SwiftUI
import UniformTypeIdentifiers

struct AudioVfsCaptureDocument: FileDocument {
    static let readableContentTypes: [UTType] = [.wav]
    let data: Data

    init(pcm: [UInt8]) {
        // The fixture captures mono, 48 kHz, signed 16-bit little-endian PCM.
        let sampleRate: UInt32 = 48_000
        let channels: UInt16 = 1
        let bitsPerSample: UInt16 = 16
        let blockAlign = channels * (bitsPerSample / 8)
        let byteCount = UInt32(pcm.count)

        var wave = Data("RIFF".utf8)
        func append<T: FixedWidthInteger>(_ value: T) {
            withUnsafeBytes(of: value.littleEndian) { wave.append(contentsOf: $0) }
        }

        append(36 + byteCount)
        wave.append(contentsOf: "WAVEfmt ".utf8)
        append(UInt32(16)) // PCM format chunk size
        append(UInt16(1)) // PCM format tag
        append(channels)
        append(sampleRate)
        append(sampleRate * UInt32(blockAlign))
        append(blockAlign)
        append(bitsPerSample)
        wave.append(contentsOf: "data".utf8)
        append(byteCount)
        wave.append(contentsOf: pcm)
        data = wave
    }

    init(configuration: ReadConfiguration) throws {
        // This document is produced only from a verified fixture capture.
        throw CocoaError(.fileReadUnknown)
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}
