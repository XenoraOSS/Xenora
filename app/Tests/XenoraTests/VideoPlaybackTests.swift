// SPDX-License-Identifier: GPL-3.0-or-later

import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
import Metal
import Testing
@testable import Xenora

@MainActor
struct VideoPlaybackTests {
    private func temporaryVideoURL() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("xenora-video-\(UUID().uuidString).mov")
    }

    private func makePixelBuffer(width: Int, height: Int, seed: UInt8) throws -> CVPixelBuffer {
        var buffer: CVPixelBuffer?
        let status = CVPixelBufferCreate(
            kCFAllocatorDefault,
            width,
            height,
            kCVPixelFormatType_32BGRA,
            nil,
            &buffer
        )
        guard status == kCVReturnSuccess, let buffer else {
            throw NSError(domain: "VideoPlaybackTests", code: Int(status))
        }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        let bytesPerRow = CVPixelBufferGetBytesPerRow(buffer)
        let base = CVPixelBufferGetBaseAddress(buffer)!
        for row in 0..<height {
            let line = base.advanced(by: row * bytesPerRow).assumingMemoryBound(to: UInt8.self)
            for column in 0..<width {
                let pixel = line.advanced(by: column * 4)
                pixel[0] = seed
                pixel[1] = UInt8(truncatingIfNeeded: Int(seed) + row)
                pixel[2] = UInt8(truncatingIfNeeded: Int(seed) + column)
                pixel[3] = 255
            }
        }
        return buffer
    }

    private func writeFixture(url: URL, frameCount: Int = 5, width: Int = 16, height: Int = 16, frameRate: Int32 = 30) async throws {
        try? FileManager.default.removeItem(at: url)
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        let settings: [String: Any] = [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
        ]
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
        input.expectsMediaDataInRealTime = false
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(
            assetWriterInput: input,
            sourcePixelBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
            ]
        )
        #expect(writer.canAdd(input))
        writer.add(input)
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for index in 0..<frameCount {
            for attempt in 0..<1000 where !input.isReadyForMoreMediaData {
                try await Task.sleep(for: .milliseconds(1))
                if attempt == 999 { Issue.record("video writer input never became ready") }
            }
            let buffer = try makePixelBuffer(width: width, height: height, seed: UInt8(index * 20 + 1))
            let time = CMTime(value: CMTimeValue(index), timescale: frameRate)
            #expect(adaptor.append(buffer, withPresentationTime: time))
        }
        input.markAsFinished()
        await writer.finishWriting()
        #expect(writer.status == .completed)
    }

    private func seconds(_ time: CMTime) -> Double {
        CMTimeGetSeconds(time)
    }

    @Test func generatedFixtureDecodesFramesInPresentationOrder() async throws {
        let url = temporaryVideoURL()
        defer { try? FileManager.default.removeItem(at: url) }
        try await writeFixture(url: url)
        let playback = try await VideoPlayback.load(url: url)
        defer { playback.close() }

        #expect(playback.state == .ready)
        #expect(abs(seconds(playback.duration) - 5.0 / 30.0) < 0.02)
        #expect(playback.naturalSize == CGSize(width: 16, height: 16))
        try playback.play()
        var timestamps: [Double] = []
        var frameDurations: [Double] = []
        while let frame = try playback.nextFrame() {
            let pixelBuffer = try #require(frame.pixelBuffer)
            #expect(CVPixelBufferGetWidth(pixelBuffer) == 16)
            #expect(CVPixelBufferGetHeight(pixelBuffer) == 16)
            #expect(CVPixelBufferGetPixelFormatType(pixelBuffer) == kCVPixelFormatType_32BGRA)
            timestamps.append(seconds(frame.presentationTime))
            frameDurations.append(seconds(frame.duration))
        }
        #expect(playback.state == .ended)
        #expect(timestamps.count == 5)
        #expect(timestamps == timestamps.sorted())
        for index in 0..<5 {
            #expect(abs(timestamps[index] - Double(index) / 30.0) < 0.02)
            #expect(abs(frameDurations[index] - 1.0 / 30.0) < 0.02)
        }
        #expect(try playback.nextFrame() == nil)
    }

    @Test func seekingRecuesTheDecoderInAPausedState() async throws {
        let url = temporaryVideoURL()
        defer { try? FileManager.default.removeItem(at: url) }
        try await writeFixture(url: url)
        let playback = try await VideoPlayback.load(url: url)
        defer { playback.close() }

        try playback.play()
        _ = try playback.nextFrame()
        try playback.pause()
        try playback.seek(to: CMTime(value: 3, timescale: 30))
        #expect(playback.state == .paused)
        try playback.play()
        let frame = try #require(try playback.nextFrame())
        #expect(abs(seconds(frame.presentationTime) - 3.0 / 30.0) < 0.04)
    }

    @Test func invalidSeekTimesAreRejectedWithoutChangingState() async throws {
        let url = temporaryVideoURL()
        defer { try? FileManager.default.removeItem(at: url) }
        try await writeFixture(url: url)
        let playback = try await VideoPlayback.load(url: url)
        defer { playback.close() }

        for time in [CMTime.invalid, CMTime(value: -1, timescale: 30), playback.duration] {
            do {
                try playback.seek(to: time)
                Issue.record("invalid seek time was accepted")
            } catch let error as VideoPlaybackError {
                #expect(error == .invalidTime)
            }
            #expect(playback.state == .ready)
        }
    }

    @Test func nonVideoInputIsRejectedAndCloseIsTerminal() async throws {
        let url = temporaryVideoURL()
        defer { try? FileManager.default.removeItem(at: url) }
        try Data("not a movie".utf8).write(to: url)
        do {
            _ = try await VideoPlayback.load(url: url)
            Issue.record("non-video input unexpectedly loaded")
        } catch {
            // AVFoundation may reject the container before track inspection.
            if let playbackError = error as? VideoPlaybackError {
                #expect(playbackError == .missingVideoTrack)
            } else {
                #expect(error is AVError)
            }
        }

        try await writeFixture(url: url)
        for _ in 0..<4 {
            let playback = try await VideoPlayback.load(url: url)
            playback.close()
            #expect(playback.state == .closed)
            do {
                try playback.play()
                Issue.record("closed playback accepted play")
            } catch let error as VideoPlaybackError {
                #expect(error == .closed)
            }
        }
    }

    @Test func pixelBufferUploadPreservesRowsAndPixelBytes() throws {
        let uploader = try VideoTextureUploader()
        let buffer = try makePixelBuffer(width: 3, height: 2, seed: 7)
        let texture = try uploader.upload(pixelBuffer: buffer)

        #expect(texture.width == 3)
        #expect(texture.height == 2)
        #expect(texture.pixelFormat == .bgra8Unorm)
        var readback = [UInt8](repeating: 0, count: 3 * 2 * 4)
        readback.withUnsafeMutableBytes { pointer in
            texture.getBytes(
                pointer.baseAddress!,
                bytesPerRow: 3 * 4,
                from: MTLRegion(
                    origin: MTLOrigin(x: 0, y: 0, z: 0),
                    size: MTLSize(width: 3, height: 2, depth: 1)
                ),
                mipmapLevel: 0
            )
        }
        var expected: [UInt8] = []
        for row in 0..<2 {
            for column in 0..<3 {
                expected.append(7)
                expected.append(UInt8(truncatingIfNeeded: 7 + row))
                expected.append(UInt8(truncatingIfNeeded: 7 + column))
                expected.append(255)
            }
        }
        #expect(readback == expected)
    }

    @Test func nonBgraPixelBuffersAreRejectedBeforeTextureCreation() throws {
        var buffer: CVPixelBuffer?
        let status = CVPixelBufferCreate(
            kCFAllocatorDefault,
            2,
            2,
            kCVPixelFormatType_32ARGB,
            nil,
            &buffer
        )
        #expect(status == kCVReturnSuccess)
        let pixelBuffer = try #require(buffer)
        do {
            _ = try VideoTextureUploader().upload(pixelBuffer: pixelBuffer)
            Issue.record("non-BGRA pixel buffer was accepted")
        } catch let error as VideoTextureUploadError {
            #expect(error == .unsupportedPixelFormat)
        }
    }

    @Test func decodedFrameUploadProducesANativeTexture() async throws {
        let url = temporaryVideoURL()
        defer { try? FileManager.default.removeItem(at: url) }
        try await writeFixture(url: url)
        let playback = try await VideoPlayback.load(url: url)
        defer { playback.close() }
        try playback.play()
        let frame = try #require(try playback.nextFrame())
        let texture = try VideoTextureUploader().upload(frame: frame)

        #expect(texture.width == 16)
        #expect(texture.height == 16)
        #expect(texture.pixelFormat == .bgra8Unorm)
        var alpha = [UInt8](repeating: 0, count: 4)
        texture.getBytes(
            &alpha,
            bytesPerRow: 4,
            from: MTLRegion(
                origin: MTLOrigin(x: 8, y: 8, z: 0),
                size: MTLSize(width: 1, height: 1, depth: 1)
            ),
            mipmapLevel: 0
        )
        #expect(alpha[3] == 255)
    }

}
