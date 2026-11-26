// SPDX-License-Identifier: GPL-3.0-or-later

import AVFoundation
import CXenora
import Foundation
import Testing
@testable import Xenora

@MainActor
struct AudioVfsCaptureTests {
    private actor Captures {
        var remaining: [AudioVfsFrame]

        init(_ frames: [AudioVfsFrame]) { remaining = frames }

        func capture() -> AudioVfsFrame { remaining.removeFirst() }
    }

    private func frame(pcm: [UInt8], error: String? = nil) -> AudioVfsFrame {
        AudioVfsFrame(status: Int32(XENORA_OK), width: 2, height: 2, samples: 256,
                      totalAudioBytes: 512, rgba: [], pcm: pcm, audioCaptureError: error)
    }

    @Test func exportPreservesThePcmAndWritesTheExactWaveHeader() async throws {
        let pcm = (0..<512).map { UInt8(truncatingIfNeeded: $0) }
        let captured = frame(pcm: pcm)
        let model = AudioVfsPlaybackModel(capture: { captured }, playPreview: { _ in })
        await model.run()
        let document = try #require(model.captureForExport())
        let header: [UInt8] = [
            0x52, 0x49, 0x46, 0x46, 0x24, 0x02, 0, 0, // RIFF and 548 bytes
            0x57, 0x41, 0x56, 0x45, 0x66, 0x6d, 0x74, 0x20, // WAVE, fmt
            16, 0, 0, 0, 1, 0, 1, 0, // 16-byte PCM format, mono
            0x80, 0xbb, 0, 0, 0, 0x77, 1, 0, // 48 kHz, 96,000 bytes/s
            2, 0, 16, 0, // two bytes per frame, 16 bits per sample
            0x64, 0x61, 0x74, 0x61, 0, 2, 0, 0, // data, 512 bytes
        ]
        #expect(document.data == Data(header + pcm))
    }

    @Test func exportUsesAnImmutableValidCapture() async throws {
        let first = [UInt8](repeating: 0x3c, count: 512)
        let second = [UInt8](repeating: 0xe7, count: 512)
        let input = Captures([frame(pcm: first, error: "Capture failed"), frame(pcm: first), frame(pcm: second)])
        let model = AudioVfsPlaybackModel(capture: { await input.capture() }, playPreview: { _ in })

        #expect(model.captureForExport() == nil)
        await model.run()
        #expect(model.captureForExport() == nil)
        await model.run()
        let snapshot = try #require(model.captureForExport())
        await model.run()
        let current = try #require(model.captureForExport())

        #expect(snapshot.data.dropFirst(44) == Data(first))
        #expect(current.data.dropFirst(44) == Data(second))
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["XENORA_NATIVE_AUDIO_EXPORT_TEST"] == "1"))
    func nativeCaptureCanBeDecodedAsWave() async throws {
        let model = AudioVfsPlaybackModel(playPreview: { _ in })
        await model.run()
        let capture = try #require(model.frame?.previewPCM)
        let document = try #require(model.captureForExport())
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("xenora-audio-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: url) }
        try document.data.write(to: url)

        let file = try AVAudioFile(forReading: url, commonFormat: .pcmFormatInt16, interleaved: false)
        #expect(file.length == 256)
        #expect(file.fileFormat.sampleRate == 48_000)
        #expect(file.fileFormat.channelCount == 1)
        #expect(file.fileFormat.streamDescription.pointee.mBitsPerChannel == 16)
        let buffer = try #require(AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: 256))
        try file.read(into: buffer)
        #expect(buffer.frameLength == 256)
        let samples = try #require(buffer.int16ChannelData)
        let actual = Array(UnsafeBufferPointer(start: samples[0], count: Int(buffer.frameLength)))
        let expected = stride(from: 0, to: capture.count, by: 2).map { offset in
            Int16(bitPattern: UInt16(capture[offset]) | (UInt16(capture[offset + 1]) << 8))
        }
        #expect(actual == expected)
    }
}
