// SPDX-License-Identifier: GPL-3.0-or-later

import AVFoundation
import CoreMedia
import CoreVideo
import Foundation

enum VideoPlaybackError: Error, Equatable {
    case missingVideoTrack
    case invalidTime
    case invalidState
    case closed
    case outputRejected
    case readerFailed
}

enum VideoPlaybackState: Equatable {
    case ready
    case playing
    case paused
    case ended
    case failed
    case closed
}

struct VideoFrame {
    let presentationTime: CMTime
    let duration: CMTime
    let sampleBuffer: CMSampleBuffer

    var pixelBuffer: CVPixelBuffer? {
        CMSampleBufferGetImageBuffer(sampleBuffer)
    }
}

/// A deliberately small AVFoundation playback/decoder lifecycle. The object is
/// main-actor isolated so state transitions and reader replacement are
/// serialized. It is not a real-time UI player and it does not synchronize audio.
@MainActor
final class VideoPlayback {
    let duration: CMTime
    let naturalSize: CGSize
    let nominalFrameRate: Float
    private let asset: AVURLAsset
    private let track: AVAssetTrack
    private var reader: AVAssetReader?
    private var output: AVAssetReaderTrackOutput?
    private(set) var state: VideoPlaybackState = .ready

    private init(asset: AVURLAsset, track: AVAssetTrack, duration: CMTime, naturalSize: CGSize, nominalFrameRate: Float) throws {
        self.asset = asset
        self.track = track
        self.duration = duration
        self.naturalSize = naturalSize
        self.nominalFrameRate = nominalFrameRate
        try configureReader(at: .zero)
    }

    static func load(url: URL) async throws -> VideoPlayback {
        let asset = AVURLAsset(url: url)
        let tracks = try await asset.load(.tracks)
        guard let track = tracks.first(where: { $0.mediaType == .video }) else {
            throw VideoPlaybackError.missingVideoTrack
        }
        let duration = try await asset.load(.duration)
        guard duration.isValid && duration.isNumeric && duration >= .zero else {
            throw VideoPlaybackError.invalidTime
        }
        let naturalSize = try await track.load(.naturalSize)
        let nominalFrameRate = try await track.load(.nominalFrameRate)
        return try VideoPlayback(
            asset: asset,
            track: track,
            duration: duration,
            naturalSize: naturalSize,
            nominalFrameRate: nominalFrameRate
        )
    }

    func play() throws {
        switch state {
        case .ready, .paused:
            state = .playing
        case .closed:
            throw VideoPlaybackError.closed
        case .playing, .ended, .failed:
            throw VideoPlaybackError.invalidState
        }
    }

    func pause() throws {
        switch state {
        case .playing:
            state = .paused
        case .closed:
            throw VideoPlaybackError.closed
        case .ready, .paused, .ended, .failed:
            throw VideoPlaybackError.invalidState
        }
    }

    /// Re-cue the decoder. Seeking deliberately enters the paused state rather
    /// than hiding a state transition inside an I/O operation.
    func seek(to time: CMTime) throws {
        if state == .closed { throw VideoPlaybackError.closed }
        if state == .failed { throw VideoPlaybackError.invalidState }
        guard time.isValid && time.isNumeric && time >= .zero && time < duration else {
            throw VideoPlaybackError.invalidTime
        }
        try configureReader(at: time)
        state = .paused
    }

    /// Decode the next uncompressed frame output as a buffer with its media
    /// timestamp. Returning nil means end of stream. A nil result is stable:
    /// subsequent calls remain at end of stream until an explicit seek.
    func nextFrame() throws -> VideoFrame? {
        if state == .closed { throw VideoPlaybackError.closed }
        if state == .ended { return nil }
        guard state == .playing else { throw VideoPlaybackError.invalidState }
        guard let output else { throw VideoPlaybackError.readerFailed }
        guard let sample = output.copyNextSampleBuffer() else {
            if reader?.status == .failed {
                state = .failed
                throw VideoPlaybackError.readerFailed
            }
            state = .ended
            return nil
        }
        let sampleDuration = CMSampleBufferGetDuration(sample)
        let fallbackDuration = nominalFrameRate > 0
            ? CMTime(seconds: 1.0 / Double(nominalFrameRate), preferredTimescale: 600)
            : CMTime.invalid
        return VideoFrame(
            presentationTime: CMSampleBufferGetPresentationTimeStamp(sample),
            duration: sampleDuration.isValid ? sampleDuration : fallbackDuration,
            sampleBuffer: sample
        )
    }

    func close() {
        reader?.cancelReading()
        reader = nil
        output = nil
        state = .closed
    }

    private func configureReader(at time: CMTime) throws {
        let nextReader = try AVAssetReader(asset: asset)
        if time > .zero && time < duration {
            nextReader.timeRange = CMTimeRange(start: time, duration: CMTimeSubtract(duration, time))
        }
        let settings: [String: Any] = [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ]
        let nextOutput = AVAssetReaderTrackOutput(track: track, outputSettings: settings)
        guard nextReader.canAdd(nextOutput) else {
            throw VideoPlaybackError.outputRejected
        }
        nextReader.add(nextOutput)
        guard nextReader.startReading() else {
            throw VideoPlaybackError.readerFailed
        }
        reader?.cancelReading()
        reader = nextReader
        output = nextOutput
    }
}
