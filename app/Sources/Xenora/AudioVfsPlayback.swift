// SPDX-License-Identifier: GPL-3.0-or-later

import CXenora
import Foundation
import SwiftUI

@MainActor
final class AudioVfsPlaybackModel: ObservableObject {
    enum Phase: Equatable {
        case idle
        case capturing
        case playing
    }

    enum PlaybackResult: Equatable {
        case completed
        case failed(String)
    }

    @Published private(set) var phase: Phase = .idle
    @Published private(set) var frame: AudioVfsFrame?
    @Published private(set) var fixtureMs: Double?
    @Published private(set) var playbackResult: PlaybackResult?

    private let capture: @Sendable () async -> AudioVfsFrame
    private let playPreview: @Sendable ([UInt8]) async throws -> Void

    init(
        capture: @escaping @Sendable () async -> AudioVfsFrame = {
            await Task.detached(priority: .userInitiated) { AudioVfsFrame.run() }.value
        },
        playPreview: @escaping @Sendable ([UInt8]) async throws -> Void
    ) {
        self.capture = capture
        self.playPreview = playPreview
    }

    var canRun: Bool { phase == .idle }
    var canListen: Bool { phase == .idle && frame?.previewPCM != nil }

    func captureForExport() -> AudioVfsCaptureDocument? {
        guard phase == .idle, let pcm = frame?.previewPCM else { return nil }
        return AudioVfsCaptureDocument(pcm: pcm)
    }

    var captureFailure: String? {
        guard let frame else { return nil }
        if frame.status != XENORA_OK {
            return "Audio/VFS fixture failed (status \(frame.status))."
        }
        return frame.audioCaptureError
    }

    func run() async {
        guard canRun else { return }
        phase = .capturing
        frame = nil
        fixtureMs = nil
        playbackResult = nil

        let started = ContinuousClock.now
        frame = await capture()
        let elapsed = started.duration(to: .now).components
        fixtureMs = Double(elapsed.seconds) * 1_000 + Double(elapsed.attoseconds) / 1e15
        phase = .idle
    }

    func listen() async {
        guard phase == .idle, let pcm = frame?.previewPCM else { return }
        phase = .playing
        playbackResult = nil
        do {
            try await playPreview(pcm)
            playbackResult = .completed
        } catch {
            playbackResult = .failed(error.localizedDescription)
        }
        phase = .idle
    }
}

struct AudioVfsFixtureView: View {
    @ObservedObject var model: AudioVfsPlaybackModel
    let connected: Bool
    let hudEnabled: Bool
    @State private var exportingAudio = false
    @State private var exportDocument: AudioVfsCaptureDocument?
    @State private var exportError: String?

    var body: some View {
        Section("Audio/VFS fixture") {
            if let frame = model.frame, let image = frame.image {
                Image(decorative: image, scale: 1)
                    .resizable()
                    .interpolation(.none)
                    .aspectRatio(contentMode: .fit)
                    .frame(maxWidth: .infinity, minHeight: 80, maxHeight: 160)
                    .metalHud(hudEnabled, [
                        "\(frame.width)x\(frame.height) audio/VFS",
                        model.fixtureMs.map { String(format: "host run %.1f ms", $0) } ?? "host run -",
                    ])
                    .accessibilityLabel("Audio/VFS guest frame, \(frame.width) by \(frame.height) pixels")
                Text("Loaded the embedded asset through the virtual filesystem and rendered it with Metal.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                if frame.previewPCM != nil {
                    Text("\(frame.samples) audio frames captured at 48 kHz.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
            }
            HStack {
                Button(model.phase == .capturing ? "Running…" : "Run Audio/VFS fixture") {
                    Task { await model.run() }
                }
                .disabled(!connected || !model.canRun)

                Button(model.phase == .playing ? "Playing…" : "Listen to fixture audio") {
                    Task { await model.listen() }
                }
                .disabled(!model.canListen)
            }
            Button("Save captured audio…") {
                guard let document = model.captureForExport() else { return }
                exportDocument = document
                exportError = nil
                exportingAudio = true
            }
            .disabled(!model.canListen)
            .accessibilityIdentifier("audio-vfs-save")
            if let exportError {
                Text(exportError).foregroundStyle(.red)
            }
            if let failure = model.captureFailure {
                Text(failure).foregroundStyle(.red)
            }
            if let result = model.playbackResult {
                switch result {
                case .completed:
                    Text("Audio preview completed.").foregroundStyle(.secondary)
                case .failed(let message):
                    Text(message).foregroundStyle(.red)
                }
            }
        }
        .fileExporter(
            isPresented: $exportingAudio,
            document: exportDocument,
            contentTypes: [.wav],
            defaultFilename: "xenora-guest-audio"
        ) { result in
            exportDocument = nil
            if case let .failure(error) = result, (error as? CocoaError)?.code != .userCancelled {
                exportError = "Could not save captured audio: \(error.localizedDescription)"
            }
        } onCancellation: {
            exportDocument = nil
        }
    }
}
