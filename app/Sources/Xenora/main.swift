// SPDX-License-Identifier: GPL-3.0-or-later
//
// Native shell over the versioned C interface. "Run trusted fixture" executes the
// bundled trusted PE fixture (x86-64 translated to ARM64; its draw request is rendered
// by native Metal) and shows the verified read-back frame. Any other executable runs
// through the bundled `xenora run` worker (ProgramRun.swift).

import AppKit
import AVFoundation
import AudioToolbox
import CoreAudio
import CXenora
import Foundation
import SwiftUI
import UniformTypeIdentifiers

struct RuntimeProbe {
    var abiVersion: UInt32 = 0
    var status: Int32 = -1
    var sections: UInt32 = 0
    var entryRVA: UInt32 = 0

    var connected: Bool { abiVersion == XENORA_ABI_VERSION && status == XENORA_OK }

    static func run() -> RuntimeProbe {
        var probe = RuntimeProbe(abiVersion: xenora_abi_version())
        var session: xenora_session = 0
        probe.status = xenora_session_create(XENORA_ABI_VERSION, &session)
        guard probe.status == XENORA_OK else { return probe }
        defer { _ = xenora_session_destroy(session) }
        probe.status = xenora_session_inspect_trusted_fixture(session, &probe.sections, &probe.entryRVA)
        return probe
    }

    var summary: String {
        String(format: "abi=%u status=%d sections=%u entry_rva=0x%x", abiVersion, status, sections, entryRVA)
    }
}

/// The frame produced by one run of the trusted fixture.
struct GuestFrame {
    let status: Int32
    let width: Int
    let height: Int
    let rgba: [UInt8]

    static func run() -> GuestFrame {
        var session: xenora_session = 0
        var status = xenora_session_create(XENORA_ABI_VERSION, &session)
        guard status == XENORA_OK else { return GuestFrame(status: status, width: 0, height: 0, rgba: []) }
        defer { _ = xenora_session_destroy(session) }
        var rgba = [UInt8](repeating: 0, count: Int(XENORA_FIXTURE_FRAME_WIDTH * XENORA_FIXTURE_FRAME_HEIGHT * 4))
        var width: UInt32 = 0
        var height: UInt32 = 0
        status = rgba.withUnsafeMutableBufferPointer { buffer in
            xenora_session_run_trusted_fixture(session, buffer.baseAddress, UInt64(buffer.count), &width, &height)
        }
        return GuestFrame(status: status, width: Int(width), height: Int(height), rgba: status == XENORA_OK ? rgba : [])
    }

    var image: CGImage? {
        guard status == XENORA_OK, let provider = CGDataProvider(data: Data(rgba) as CFData) else { return nil }
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }

    var summary: String {
        let first = rgba.prefix(4).map(String.init).joined(separator: ",")
        return "status=\(status) frame=\(width)x\(height) first_pixel=\(first)"
    }
}

/// The frame produced by running the textured fixture (Gate 5, #279).
struct TexturedFrame {
    let status: Int32
    let width: Int
    let height: Int
    let rgba: [UInt8]

    static func run() -> TexturedFrame {
        var session: xenora_session = 0
        var status = xenora_session_create(XENORA_ABI_VERSION, &session)
        guard status == XENORA_OK else { return TexturedFrame(status: status, width: 0, height: 0, rgba: []) }
        defer { _ = xenora_session_destroy(session) }
        var rgba = [UInt8](repeating: 0, count: 16)
        var width: UInt32 = 0
        var height: UInt32 = 0
        status = rgba.withUnsafeMutableBufferPointer { buffer in
            xenora_session_run_textured_fixture(session, buffer.baseAddress, UInt64(buffer.count), &width, &height)
        }
        return TexturedFrame(status: status, width: Int(width), height: Int(height), rgba: status == XENORA_OK ? rgba : [])
    }

    var image: CGImage? {
        guard status == XENORA_OK, let provider = CGDataProvider(data: Data(rgba) as CFData) else { return nil }
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }

    var summary: String {
        let pixels = rgba.map(String.init).joined(separator: ",")
        return "status=\(status) frame=\(width)x\(height) pixels=[\(pixels)]"
    }
}

/// The frame and audio status produced by running the audio/VFS fixture (Gate 6, #284).
struct AudioVfsFrame {
    let status: Int32
    let width: Int
    let height: Int
    let samples: UInt32
    let totalAudioBytes: UInt64
    let rgba: [UInt8]
    let pcm: [UInt8]

    static func run(captureAudio: Bool = false) -> AudioVfsFrame {
        var session: xenora_session = 0
        var status = xenora_session_create(XENORA_ABI_VERSION, &session)
        guard status == XENORA_OK else {
            return AudioVfsFrame(status: status, width: 0, height: 0, samples: 0, totalAudioBytes: 0, rgba: [], pcm: [])
        }
        defer { _ = xenora_session_destroy(session) }
        var rgba = [UInt8](repeating: 0, count: 16)
        var width: UInt32 = 0
        var height: UInt32 = 0
        var samples: UInt32 = 0
        status = rgba.withUnsafeMutableBufferPointer { buffer in
            xenora_session_run_audio_vfs_fixture(
                session, buffer.baseAddress, UInt64(buffer.count), &width, &height, &samples)
        }
        var queued: UInt32 = 0
        var underruns: UInt32 = 0
        var total: UInt64 = 0
        _ = xenora_session_audio_status(session, &queued, &underruns, &total)
        var pcm: [UInt8] = []
        if captureAudio && status == XENORA_OK && queued > 0 && queued <= 256 * 1024 {
            pcm = [UInt8](repeating: 0, count: Int(queued))
            var read: UInt64 = 0
            let readStatus = pcm.withUnsafeMutableBufferPointer { buffer in
                xenora_session_audio_read(session, buffer.baseAddress, UInt64(buffer.count), &read)
            }
            if readStatus != XENORA_OK || read != UInt64(queued) {
                pcm.removeAll()
            }
        }
        return AudioVfsFrame(
            status: status,
            width: Int(width),
            height: Int(height),
            samples: samples,
            totalAudioBytes: total,
            rgba: status == XENORA_OK ? rgba : [],
            pcm: pcm
        )
    }

    var image: CGImage? {
        guard status == XENORA_OK, let provider = CGDataProvider(data: Data(rgba) as CFData) else { return nil }
        return CGImage(
            width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }

    var summary: String {
        let pixels = rgba.map(String.init).joined(separator: ",")
        return "status=\(status) frame=\(width)x\(height) samples=\(samples) audio_total=\(totalAudioBytes) pixels=[\(pixels)]"
    }
}

private enum FixturePlaybackError: Error {
    case invalidPCM
    case noDefaultOutput(OSStatus)
    case deviceDiscovery(OSStatus)
    case unavailableOutput(AudioDeviceID)
    case outputUnitUnavailable
    case outputRouting(OSStatus)
    case unexpectedOutput(AudioDeviceID)
    case queueFailure(Int32)
    case queueShortfall(UInt64)
    case timedOut
}

private struct FixturePlayback {
    let frames: Int
    let buffers: Int
    let refills: Int
    let underruns: UInt32
    let outputDevice: AudioDeviceID
    let outputSampleRate: Double
    let outputChannels: AVAudioChannelCount
    let presentationLatency: TimeInterval
    let completionElapsed: TimeInterval
}

private func defaultAudioOutput() throws -> AudioDeviceID {
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDefaultOutputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var device = AudioDeviceID(kAudioObjectUnknown)
    var size = UInt32(MemoryLayout<AudioDeviceID>.size)
    let status = AudioObjectGetPropertyData(
        AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &device
    )
    guard status == noErr, device != kAudioObjectUnknown else {
        throw FixturePlaybackError.noDefaultOutput(status)
    }
    return device
}

private func audioOutputDevices() throws -> [AudioDeviceID] {
    let system = AudioObjectID(kAudioObjectSystemObject)
    var address = AudioObjectPropertyAddress(
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    let stride = UInt32(MemoryLayout<AudioDeviceID>.stride)
    var size: UInt32 = 0
    let sizeStatus = AudioObjectGetPropertyDataSize(system, &address, 0, nil, &size)
    guard sizeStatus == noErr, size.isMultiple(of: stride) else {
        throw FixturePlaybackError.deviceDiscovery(sizeStatus)
    }
    if size == 0 { return [] }
    var devices = [AudioDeviceID](repeating: kAudioObjectUnknown, count: Int(size / stride))
    let readStatus = devices.withUnsafeMutableBytes { bytes in
        AudioObjectGetPropertyData(system, &address, 0, nil, &size, bytes.baseAddress!)
    }
    guard readStatus == noErr, size.isMultiple(of: stride), Int(size / stride) <= devices.count else {
        throw FixturePlaybackError.deviceDiscovery(readStatus)
    }
    return devices.prefix(Int(size / stride)).filter { device in
        var streams = AudioObjectPropertyAddress(
            mSelector: kAudioDevicePropertyStreams,
            mScope: kAudioDevicePropertyScopeOutput,
            mElement: kAudioObjectPropertyElementMain
        )
        var bytes: UInt32 = 0
        return device != kAudioObjectUnknown
            && AudioObjectGetPropertyDataSize(device, &streams, 0, nil, &bytes) == noErr
            && bytes > 0
    }.sorted()
}

private func playFixturePCM(
    _ pcm: [UInt8], requestedOutput: AudioDeviceID? = nil, streamingFixture: Bool = false
) throws -> FixturePlayback {
    // The embedded audio/VFS fixture submits mono 48 kHz S16LE frames.
    guard !pcm.isEmpty, pcm.count.isMultiple(of: 2),
          let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 1) else {
        throw FixturePlaybackError.invalidPCM
    }
    let sourceFrames = pcm.count / 2
    let frames = sourceFrames * (streamingFixture ? 32 : 1)
    let framesPerBuffer = streamingFixture ? 1024 : 128
    let bufferWindow = 3
    var streamSession: xenora_session = 0
    let producer = DispatchQueue(label: "xenora.fixture.pcm")
    if streamingFixture {
        let createStatus = xenora_session_create(XENORA_ABI_VERSION, &streamSession)
        guard createStatus == XENORA_OK else { throw FixturePlaybackError.queueFailure(createStatus) }
    }
    let session = streamSession
    defer {
        if session != 0 {
            producer.sync {}
            _ = xenora_session_destroy(session)
        }
    }
    if session != 0 {
        let initStatus = xenora_session_audio_init(session, 48_000, 1, UInt32(XENORA_AUDIO_FORMAT_PCM_S16LE))
        guard initStatus == XENORA_OK else { throw FixturePlaybackError.queueFailure(initStatus) }
    }
    let engine = AVAudioEngine()
    guard let outputUnit = engine.outputNode.audioUnit else {
        throw FixturePlaybackError.outputUnitUnavailable
    }
    if var device = requestedOutput {
        guard try audioOutputDevices().contains(device) else {
            throw FixturePlaybackError.unavailableOutput(device)
        }
        let status = AudioUnitSetProperty(
            outputUnit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0,
            &device, UInt32(MemoryLayout<AudioDeviceID>.size)
        )
        guard status == noErr else { throw FixturePlaybackError.outputRouting(status) }
    }
    let player = AVAudioPlayerNode()
    let played = DispatchSemaphore(value: 0)
    engine.attach(player)
    engine.connect(player, to: engine.mainMixerNode, format: format)
    defer {
        player.stop()
        engine.stop()
        engine.detach(player)
    }
    try engine.start()
    var outputDevice = AudioDeviceID(kAudioObjectUnknown)
    var outputIDSize = UInt32(MemoryLayout<AudioDeviceID>.size)
    let routeStatus = AudioUnitGetProperty(
        outputUnit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0,
        &outputDevice, &outputIDSize
    )
    guard routeStatus == noErr else { throw FixturePlaybackError.outputRouting(routeStatus) }
    guard outputDevice != kAudioObjectUnknown, requestedOutput.map({ $0 == outputDevice }) ?? true else {
        throw FixturePlaybackError.unexpectedOutput(outputDevice)
    }
    let output = engine.outputNode.outputFormat(forBus: 0)
    let presentationLatency = engine.outputNode.presentationLatency
    var nextFrame = 0
    var bufferCount = 0
    func renderBlock(first: Int, count: Int) throws -> [UInt8] {
        let firstByte = first * 2
        let byteCount = count * 2
        if session == 0 {
            return Array(pcm[firstByte..<(firstByte + byteCount)])
        }
        let submitted = DispatchSemaphore(value: 0)
        producer.async {
            let block = (0..<byteCount).map { pcm[(firstByte + $0) % pcm.count] }
            block.withUnsafeBufferPointer { bytes in
                _ = xenora_session_audio_submit(session, bytes.baseAddress, UInt64(bytes.count))
            }
            submitted.signal()
        }
        guard submitted.wait(timeout: .now() + .seconds(1)) == .success else {
            throw FixturePlaybackError.timedOut
        }
        var block = [UInt8](repeating: 0, count: byteCount)
        var supplied: UInt64 = 0
        let status = block.withUnsafeMutableBufferPointer { bytes in
            xenora_session_audio_render(session, bytes.baseAddress, UInt64(bytes.count), &supplied)
        }
        guard status == XENORA_OK else { throw FixturePlaybackError.queueFailure(status) }
        guard supplied == UInt64(byteCount) else { throw FixturePlaybackError.queueShortfall(supplied) }
        return block
    }

    func scheduleNextBuffer() throws {
        let first = nextFrame
        let count = min(framesPerBuffer, frames - first)
        let pcmBlock = try renderBlock(first: first, count: count)
        guard let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(count)),
              let samples = buffer.floatChannelData?[0] else {
            throw FixturePlaybackError.invalidPCM
        }
        buffer.frameLength = AVAudioFrameCount(count)
        for offset in 0..<count {
            let byte = offset * 2
            let low = UInt16(pcmBlock[byte])
            let high = UInt16(pcmBlock[byte + 1]) << 8
            samples[offset] = Float(Int16(bitPattern: low | high)) / 32_768
        }
        player.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { _ in
            played.signal()
        }
        bufferCount += 1
        nextFrame += count
    }
    while nextFrame < frames && bufferCount < bufferWindow {
        try scheduleNextBuffer()
    }
    let prequeued = bufferCount
    let start = DispatchTime.now()
    player.play()
    let deadline = start + .seconds(3)
    var completedBuffers = 0
    while completedBuffers < bufferCount {
        guard played.wait(timeout: deadline) == .success else {
            throw FixturePlaybackError.timedOut
        }
        completedBuffers += 1
        if nextFrame < frames {
            try scheduleNextBuffer()
        }
    }
    let elapsed = Double(DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds) / 1e9
    var underruns: UInt32 = 0
    if session != 0 {
        var queued: UInt32 = 0
        var total: UInt64 = 0
        let status = xenora_session_audio_status(session, &queued, &underruns, &total)
        guard status == XENORA_OK else { throw FixturePlaybackError.queueFailure(status) }
        guard queued == 0, total == UInt64(frames * 2) else {
            throw FixturePlaybackError.invalidPCM
        }
    }
    return FixturePlayback(
        frames: frames, buffers: completedBuffers, refills: bufferCount - prequeued,
        underruns: underruns, outputDevice: outputDevice,
        outputSampleRate: output.sampleRate, outputChannels: output.channelCount,
        presentationLatency: presentationLatency, completionElapsed: elapsed
    )
}

/// Owns the latest fixture run. A class with @Published avoids SwiftUI macros,
/// which the command-line toolchain does not ship.
@MainActor
final class FrameModel: ObservableObject {
    @Published var frame: GuestFrame?
    @Published var running = false
    @Published var texturedFrame: TexturedFrame?
    @Published var runningTextured = false
    @Published var audioVfsFrame: AudioVfsFrame?
    @Published var runningAudioVfs = false
    /// Host wall time of the latest run of each fixture, measured here, for the HUD.
    @Published var frameMs: Double?
    @Published var texturedMs: Double?
    @Published var audioVfsMs: Double?

    private static func timed<T: Sendable>(_ body: @escaping @Sendable () -> T) async -> (T, Double) {
        await Task.detached {
            let started = Date()
            let value = body()
            return (value, Date().timeIntervalSince(started) * 1000)
        }.value
    }

    func run() {
        running = true
        Task {
            // The fixture blocks while it executes and renders, so keep it off the main thread.
            (frame, frameMs) = await Self.timed { GuestFrame.run() }
            running = false
        }
    }

    func runTextured() {
        runningTextured = true
        Task {
            (texturedFrame, texturedMs) = await Self.timed { TexturedFrame.run() }
            runningTextured = false
        }
    }

    func runAudioVfs() {
        runningAudioVfs = true
        Task {
            (audioVfsFrame, audioVfsMs) = await Self.timed { AudioVfsFrame.run() }
            runningAudioVfs = false
        }
    }
}

/// Read-only compatibility inspection of a user-chosen file. Nothing in it is executed.
@MainActor
final class InspectionModel: ObservableObject {
    static let shared = InspectionModel()
    @Published var fileName: String?
    /// The inspected file, which the Run action runs.
    @Published var url: URL?
    @Published var report = ""
    @Published var status: Int32 = Int32(XENORA_OK)
    @Published var busy = false
    @Published var importing = false
    /// Why the last file selection could not be used, shown until the next selection.
    @Published var selectionError: String?

    /// Shows the read-only file importer, from the button or File > Open Executable….
    func requestImport() {
        selectionError = nil
        importing = true
    }

    /// Handles the importer's completion. A cancelled picker is not a failure.
    func importFinished(_ result: Result<URL, Error>) {
        switch result {
        case .success(let url):
            inspect(url)
        case .failure(let error):
            if let cocoa = error as? CocoaError, cocoa.code == .userCancelled {
                return
            }
            selectionError = "The selected file could not be opened: \(error.localizedDescription)"
        }
    }

    /// Reads and reports one file off the main thread. Replaceable only for the
    /// `--smoke-inspection-order` check.
    private let reader: @Sendable (URL) -> (Int32, String)
    /// Number of the latest `inspect` call. Only that call may publish a result.
    private var latestRequest: UInt64 = 0

    init(reader: @escaping @Sendable (URL) -> (Int32, String) = InspectionModel.run) {
        self.reader = reader
    }

    var failed: Bool { !busy && fileName != nil && status != Int32(XENORA_OK) }

    func inspect(_ url: URL) {
        latestRequest &+= 1
        let request = latestRequest
        selectionError = nil
        fileName = url.lastPathComponent
        self.url = url
        ProgramRunModel.shared.clear()
        report = ""
        status = Int32(XENORA_OK)
        busy = true
        let reader = reader
        Task {
            let (newStatus, newReport) = await Task.detached { reader(url) }.value
            // A newer selection owns the published state, including `busy`;
            // an older read that finishes late is discarded.
            guard request == latestRequest else { return }
            status = newStatus
            report = newReport
            busy = false
        }
    }

    nonisolated static func run(_ url: URL) -> (Int32, String) {
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        // Check the size before reading so an oversized file is never loaded into memory.
        guard let size = (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize else {
            return (Int32(XENORA_INVALID_ARGUMENT), "Could not read the file's size.")
        }
        guard UInt64(size) <= UInt64(XENORA_MAX_INSPECT_BYTES) else {
            return (Int32(XENORA_LIMIT_EXCEEDED), "The file is \(size) bytes; inspection accepts at most \(XENORA_MAX_INSPECT_BYTES).")
        }
        guard let data = try? Data(contentsOf: url) else {
            return (Int32(XENORA_INVALID_ARGUMENT), "Could not read the file.")
        }
        return data.withUnsafeBytes { raw in
            let bytes = raw.bindMemory(to: UInt8.self).baseAddress
            var required: UInt64 = 0
            var probe: CChar = 0
            var status = xenora_inspect_executable(bytes, UInt64(data.count), &probe, 1, &required)
            guard status == Int32(XENORA_BUFFER_TOO_SMALL) else { return (status, "Inspection failed (status \(status)).") }
            var text = [CChar](repeating: 0, count: Int(required))
            status = xenora_inspect_executable(bytes, UInt64(data.count), &text, required, &required)
            return (status, String(cString: text))
        }
    }
}

/// Receives files opened from Finder (double-click or Open With).
final class AppDelegate: NSObject, NSApplicationDelegate {
    func application(_ application: NSApplication, open urls: [URL]) {
        guard let url = urls.first else { return }
        MainActor.assumeIsolated { InspectionModel.shared.inspect(url) }
    }
}

struct InspectorView: View {
    @ObservedObject var model: InspectionModel

    var body: some View {
        Section("Executable inspection (read-only)") {
            HStack {
                Button("Open Executable…") { model.requestImport() }
                Text("or drop a file on this window").foregroundStyle(.secondary)
            }
            if let error = model.selectionError {
                Text(error).foregroundStyle(.red)
            }
            if let name = model.fileName {
                LabeledContent("File", value: model.busy ? "\(name) (inspecting…)" : name)
                if model.failed {
                    Text("Inspection of \(name) failed (status \(model.status)).")
                        .foregroundStyle(.red)
                }
                ScrollView {
                    Text(model.report)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(minHeight: 160, maxHeight: 320)
            }
            Text("Inspection is read-only; nothing in the file runs until you choose Run below.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }
}

struct ContentView: View {
    let probe = RuntimeProbe.run()
    @ObservedObject var model: FrameModel
    @ObservedObject var loop: FrameLoopModel
    @ObservedObject var inspection = InspectionModel.shared
    @ObservedObject var programRun = ProgramRunModel.shared
    @ObservedObject var hud = HudSettings.shared
    private var frame: GuestFrame? { model.frame }

    private func ms(_ value: Double?) -> String {
        value.map { String(format: "host run %.1f ms", $0) } ?? "host run -"
    }
    private var running: Bool { model.running }

    var body: some View {
        Form {
            InspectorView(model: inspection)
            ProgramRunView(url: inspection.url, model: programRun, hud: hud)
            Section("Display") {
                Toggle("Show Metal HUD", isOn: $hud.enabled)
                Text("Overlays the Metal device, frame size, frame counters and host-measured timings on guest frames. Program runs get startup stage traces instead, because they have no draw path yet.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Guest display") {
                if let image = frame?.image {
                    Image(decorative: image, scale: 1)
                        .resizable()
                        .interpolation(.none)
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: .infinity, minHeight: 120, maxHeight: 240)
                        .metalHud(hud.enabled, ["\(frame!.width)x\(frame!.height) offscreen, 1 draw", ms(model.frameMs)])
                        .accessibilityLabel("Guest frame, \(frame!.width) by \(frame!.height) pixels")
                    Text("Guest code ran, its draw was rendered by native Metal, and all \(frame!.width * frame!.height) pixels matched the fixture shader's output.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else if let frame {
                    Text("Run failed (status \(frame.status)). No frame is shown.")
                        .foregroundStyle(.red)
                }
                if let tImage = model.texturedFrame?.image {
                    Image(decorative: tImage, scale: 1)
                        .resizable()
                        .interpolation(.none)
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: .infinity, minHeight: 80, maxHeight: 160)
                        .metalHud(hud.enabled, ["2x2 textured", ms(model.texturedMs)])
                        .accessibilityLabel("Textured guest frame, 2 by 2 pixels")
                    Text("Textured fixture ran: persistent texture created, dynamically updated, rendered on Metal, and retired.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                if let aImage = model.audioVfsFrame?.image {
                    Image(decorative: aImage, scale: 1)
                        .resizable()
                        .interpolation(.none)
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: .infinity, minHeight: 80, maxHeight: 160)
                        .metalHud(hud.enabled, ["2x2 audio/VFS", ms(model.audioVfsMs)])
                        .accessibilityLabel("Audio/VFS guest frame, 2 by 2 pixels")
                    Text("Audio/VFS fixture ran: asset.bin loaded through sandboxed VFS, \(model.audioVfsFrame!.samples) audio samples queued (\(model.audioVfsFrame!.totalAudioBytes) bytes), texture rendered on Metal.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                HStack {
                    Button(running ? "Running…" : "Run trusted fixture") { model.run() }
                        .disabled(running || !probe.connected)
                    Button(model.runningTextured ? "Running…" : "Run textured fixture") { model.runTextured() }
                        .disabled(model.runningTextured || !probe.connected)
                    Button(model.runningAudioVfs ? "Running…" : "Run Audio/VFS fixture") { model.runAudioVfs() }
                        .disabled(model.runningAudioVfs || !probe.connected)
                }
            }
            Section("Persistent frame loop") {
                if let image = loop.image {
                    Image(decorative: image, scale: 1)
                        .resizable()
                        .interpolation(.none)
                        .aspectRatio(contentMode: .fit)
                        .frame(maxWidth: .infinity, minHeight: 120, maxHeight: 240)
                        .metalHud(hud.enabled, [
                            "\(XENORA_FIXTURE_FRAME_WIDTH)x\(XENORA_FIXTURE_FRAME_HEIGHT) frame #\(loop.frameId), \(loop.completed) completed",
                            loop.hostFps.map { String(format: "%.1f frames/s (host)", $0) } ?? "frames/s -",
                        ])
                        .accessibilityLabel("Guest frame #\(loop.frameId)")
                    Text("Frame #\(loop.frameId) (\(loop.completed) completed) • \(loop.stateDescription)")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                } else if loop.state != UInt32(XENORA_LOOP_NONE) {
                    Text("State: \(loop.stateDescription) • \(loop.completed) completed")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                }
                if let error = loop.errorMessage {
                    Text(error).foregroundStyle(.red)
                }
                HStack {
                    Button("Start 300 frames") { loop.start(frameCount: 300) }
                        .disabled(!loop.canStart || !probe.connected)
                    Button("Interactive (300 frames)") { loop.startInteractive(frameCount: 300) }
                        .disabled(!loop.canStart || !probe.connected)
                    Button("Pause") { loop.pause() }
                        .disabled(!loop.isRunning)
                    Button("Resume") { loop.resume() }
                        .disabled(!loop.isPaused)
                    Button("Stop") { loop.stop() }
                        .disabled(!loop.isRunning && !loop.isPaused)
                }
                if loop.isInteractive && (loop.isRunning || loop.isPaused) {
                    if let controller = loop.controller {
                        Text("Controller: \(controller.name)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    HStack {
                        Button("◀ Left") { loop.sendInput(buttons: UInt32(XENORA_INPUT_LEFT)) }
                        Button("▶ Right") { loop.sendInput(buttons: UInt32(XENORA_INPUT_RIGHT)) }
                        Button("Release") { loop.sendInput(buttons: UInt32(XENORA_INPUT_NONE)) }
                    }
                    Text("Interactive guest: steer the rendered strip across columns 0..3 with Left/Right.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            LabeledContent("Runtime", value: probe.connected ? "Connected" : "Unavailable (status \(probe.status))")
            LabeledContent("Interface version", value: "\(probe.abiVersion)")
            LabeledContent("Trusted fixture sections", value: "\(probe.sections)")
            LabeledContent("Trusted fixture entry RVA", value: String(format: "0x%X", probe.entryRVA))
            Text("The bundled fixtures run in the app. Other executables run through the bundled xenora runner and show their output; they do not draw yet. Presentation uses read-back Metal frames.")
                .foregroundStyle(.secondary)
        }
        .formStyle(.grouped)
        .fileImporter(isPresented: $inspection.importing, allowedContentTypes: [.item]) { result in
            inspection.importFinished(result)
        }
        .dropDestination(for: URL.self) { urls, _ in
            guard let url = urls.first, url.isFileURL else { return false }
            inspection.inspect(url)
            return true
        }
        .frame(minWidth: 520, minHeight: 680)
    }
}

struct XenoraApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate
    @ObservedObject var hud = HudSettings.shared
    let model = FrameModel()
    let loop = FrameLoopModel()

    var body: some Scene {
        WindowGroup("Xenora") { ContentView(model: model, loop: loop) }
            .commands {
                CommandGroup(after: .newItem) {
                    Button("Open Executable…") { InspectionModel.shared.requestImport() }
                        .keyboardShortcut("o")
                }
                CommandGroup(after: .toolbar) {
                    Toggle("Show Metal HUD", isOn: $hud.enabled)
                        .keyboardShortcut("h", modifiers: [.command, .shift])
                }
            }
    }
}

// `--inspect <path>` prints the read-only report for a file and exits with its status.
if let flag = CommandLine.arguments.firstIndex(of: "--inspect"), flag + 1 < CommandLine.arguments.count {
    let (status, report) = InspectionModel.run(URL(fileURLWithPath: CommandLine.arguments[flag + 1]))
    print(report.hasSuffix("\n") ? report : report + "\n", terminator: "")
    print("status=\(status)")
    exit(status == Int32(XENORA_OK) ? 0 : 1)
}

// `--smoke-resources` tests persistent texture lifecycle, dynamic subregion updates, and retirement on Metal.
if CommandLine.arguments.contains("--smoke-resources") {
    let frame = TexturedFrame.run()
    print("smoke-resources: \(frame.summary)")
    let expected: [UInt8] = [255, 255, 0, 255, 0, 255, 255, 255, 255, 0, 255, 255, 64, 64, 64, 255]
    exit(frame.status == XENORA_OK && frame.width == 2 && frame.height == 2 && frame.rgba == expected ? 0 : 1)
}

// `--smoke-audio-vfs` tests VFS asset loading, PCM audio streaming, and Metal texture presentation.
if CommandLine.arguments.contains("--smoke-controller") {
    let simulated = ControllerSnapshot(buttons: 1, x: 0.1, y: -1, leftTrigger: 1.2, rightTrigger: .nan)
    let physical = pollGameController()
    print("smoke-controller: simulatedAxis=\(simulated.x),\(simulated.y) triggers=\(simulated.leftTrigger),\(simulated.rightTrigger) detected=\(physical?.name ?? "none")")
    exit(simulated.buttons == 1 && simulated.x == 0 && simulated.y == -32767
         && simulated.leftTrigger == 1 && simulated.rightTrigger == 0 ? 0 : 1)
}

if CommandLine.arguments.contains("--smoke-audio-mix") {
    do {
        let result = try measureAudioMixFixture()
        print("smoke-audio-mix: frames=\(result.frames) mean=\(result.mean) toneAmplitude=\(result.toneAmplitude)")
        exit(result.frames == 4_800 && abs(result.mean - 0.125) < 0.005
             && abs(result.toneAmplitude - 0.25) < 0.005 ? 0 : 1)
    } catch {
        print("smoke-audio-mix: \(error)")
        exit(1)
    }
}

if CommandLine.arguments.contains("--list-audio-outputs") {
    do {
        let devices = try audioOutputDevices()
        let defaultDevice = try? defaultAudioOutput()
        print("audio-outputs: count=\(devices.count)")
        for device in devices {
            print("audio-output: id=\(device) default=\(device == defaultDevice)")
        }
        exit(0)
    } catch {
        print("audio-outputs: \(error)")
        exit(1)
    }
}

let nativeAudioStreamSmoke = CommandLine.arguments.contains("--smoke-native-audio-stream")
if CommandLine.arguments.contains("--smoke-native-audio") || nativeAudioStreamSmoke {
    var requestedOutput: AudioDeviceID?
    if let index = CommandLine.arguments.firstIndex(of: "--audio-output") {
        let values = CommandLine.arguments
        guard index + 1 < values.count, let device = AudioDeviceID(values[index + 1]), device != 0 else {
            print("smoke-native-audio: --audio-output requires a device ID")
            exit(2)
        }
        requestedOutput = device
    }
    let frame = AudioVfsFrame.run(captureAudio: true)
    guard frame.status == XENORA_OK, frame.samples == 256, frame.pcm.count == 512 else {
        print("smoke-native-audio: fixture PCM unavailable")
        exit(1)
    }
    do {
        let played = try playFixturePCM(
            frame.pcm, requestedOutput: requestedOutput, streamingFixture: nativeAudioStreamSmoke
        )
        print("smoke-native-audio: playedBackFrames=\(played.frames) playedBackBuffers=\(played.buffers) refills=\(played.refills) queueUnderruns=\(played.underruns) outputDevice=\(played.outputDevice) outputRate=\(played.outputSampleRate) outputChannels=\(played.outputChannels) presentationLatencyMs=\(played.presentationLatency * 1000) completionElapsedMs=\(played.completionElapsed * 1000)")
        let expected = nativeAudioStreamSmoke ? (8192, 8, 5) : (256, 2, 0)
        exit((played.frames, played.buffers, played.refills) == expected
             && played.underruns == 0 && played.outputSampleRate > 0 && played.outputChannels > 0 ? 0 : 1)
    } catch {
        print("smoke-native-audio: \(error)")
        exit(1)
    }
}

if CommandLine.arguments.contains("--smoke-audio-vfs") {
    let frame = AudioVfsFrame.run()
    print("smoke-audio-vfs: \(frame.summary)")
    let expected: [UInt8] = [255, 128, 0, 255, 128, 0, 255, 255, 0, 255, 128, 255, 255, 0, 128, 255]
    exit(frame.status == XENORA_OK && frame.width == 2 && frame.height == 2 && frame.samples == 256 && frame.rgba == expected && frame.totalAudioBytes == 512 ? 0 : 1)
}

// `--smoke-interactive` runs 60 frames with queued inputs on the Metal device and exits.
if CommandLine.arguments.contains("--smoke-interactive") {
    var session: xenora_session = 0
    guard xenora_session_create(XENORA_ABI_VERSION, &session) == XENORA_OK else {
        print("failed to create session")
        exit(1)
    }
    defer { _ = xenora_session_destroy(session) }
    guard xenora_session_start_interactive_loop(session, 60) == XENORA_OK else {
        print("failed to start interactive loop")
        exit(1)
    }
    for f in 0..<60 {
        let btn: UInt32 = (f < 15) ? UInt32(XENORA_INPUT_RIGHT) :
                          (f < 30) ? UInt32(XENORA_INPUT_NONE) :
                          (f < 45) ? UInt32(XENORA_INPUT_LEFT) : UInt32(XENORA_INPUT_RIGHT)
        var input = xenora_input_state(buttons: btn, stick_x: 0, stick_y: 0, mouse_dx: 0, mouse_dy: 0, mouse_buttons: 0, connected: 1, reserved: 0)
        _ = xenora_session_queue_input(session, &input)
    }
    var state: UInt32 = 0
    var completed: UInt32 = 0
    var frameId: UInt32 = 0
    var rgba = [UInt8](repeating: 0, count: Int(XENORA_FIXTURE_FRAME_WIDTH * XENORA_FIXTURE_FRAME_HEIGHT * 4))
    while true {
        let code = rgba.withUnsafeMutableBufferPointer { buf in
            xenora_session_frame_loop_status(session, &state, &completed, &frameId, buf.baseAddress, UInt64(buf.count))
        }
        if code != XENORA_OK {
            print("frame_loop_status failed: \(code)")
            exit(1)
        }
        if state != UInt32(XENORA_LOOP_RUNNING) { break }
        Thread.sleep(forTimeInterval: 0.005)
    }
    print("interactive: state=\(state) completed=\(completed) frame_id=\(frameId)")
    exit(state == UInt32(XENORA_LOOP_FINISHED) && completed == 60 ? 0 : 1)
}

// `--smoke-loop` runs 300 frames on the Metal device and exits; it needs a GPU.
if CommandLine.arguments.contains("--smoke-loop") {
    var session: xenora_session = 0
    guard xenora_session_create(XENORA_ABI_VERSION, &session) == XENORA_OK else {
        print("failed to create session")
        exit(1)
    }
    defer { _ = xenora_session_destroy(session) }
    guard xenora_session_start_frame_loop(session, 300) == XENORA_OK else {
        print("failed to start frame loop")
        exit(1)
    }
    var state: UInt32 = 0
    var completed: UInt32 = 0
    var frameId: UInt32 = 0
    var rgba = [UInt8](repeating: 0, count: Int(XENORA_FIXTURE_FRAME_WIDTH * XENORA_FIXTURE_FRAME_HEIGHT * 4))
    while true {
        let code = rgba.withUnsafeMutableBufferPointer { buf in
            xenora_session_frame_loop_status(session, &state, &completed, &frameId, buf.baseAddress, UInt64(buf.count))
        }
        if code != XENORA_OK {
            print("frame_loop_status failed: \(code)")
            exit(1)
        }
        if state != UInt32(XENORA_LOOP_RUNNING) { break }
        Thread.sleep(forTimeInterval: 0.005)
    }
    print("state=\(state) completed=\(completed) frame_id=\(frameId)")
    exit(state == UInt32(XENORA_LOOP_FINISHED) && completed == 300 ? 0 : 1)
}

// `--smoke-frame` runs the trusted fixture on the Metal device and exits; it needs a GPU.
if CommandLine.arguments.contains("--smoke-frame") {
    let frame = GuestFrame.run()
    print(frame.summary)
    exit(frame.status == XENORA_OK ? 0 : 1)
}

// `--smoke-inspection-order` checks that only the latest selection publishes an
// inspection result. Files are never read: an injected reader answers after a delay
// named by the file. It needs no GPU.
if CommandLine.arguments.contains("--smoke-inspection-order") {
    let reader: @Sendable (URL) -> (Int32, String) = { url in
        // "<delay in ms>-<ok|fail>-<tag>"
        let parts = url.lastPathComponent.split(separator: "-")
        Thread.sleep(forTimeInterval: Double(parts[0])! / 1000)
        let status = parts[1] == "ok" ? Int32(XENORA_OK) : Int32(XENORA_INVALID_ARGUMENT)
        return (status, "report \(url.lastPathComponent)")
    }
    func file(_ name: String) -> URL { URL(fileURLWithPath: "/nonexistent/\(name)") }
    func pause(_ ms: UInt64) async { try? await Task.sleep(nanoseconds: ms * 1_000_000) }

    Task { @MainActor in
        var failures: [String] = []
        @MainActor func expect(_ model: InspectionModel, _ name: String, busy: Bool, report: String, ok: Bool, _ label: String) {
            let actual = "file=\(model.fileName ?? "-") busy=\(model.busy) status=\(model.status) report=\(model.report)"
            if model.fileName != name || model.busy != busy || model.report != report
                || (model.status == Int32(XENORA_OK)) != ok {
                failures.append("\(label): \(actual)")
            }
        }
        // A slow first read finishing after a fast second one, with either outcome.
        for (first, second, ok) in [("400-ok-a", "0-ok-b", true), ("400-fail-a", "0-ok-b", true),
                                    ("400-ok-a", "0-fail-b", false)] {
            let model = InspectionModel(reader: reader)
            model.inspect(file(first))
            model.inspect(file(second))
            await pause(700)
            expect(model, second, busy: false, report: "report \(second)", ok: ok, "\(first) then \(second)")
        }
        // An older read that finishes first must not clear the newer request's busy state.
        let model = InspectionModel(reader: reader)
        model.inspect(file("100-ok-a"))
        model.inspect(file("500-fail-b"))
        await pause(300)
        expect(model, "500-fail-b", busy: true, report: "", ok: true, "while the newer read runs")
        await pause(500)
        expect(model, "500-fail-b", busy: false, report: "report 500-fail-b", ok: false, "after the newer read")
        for failure in failures { print("smoke-inspection-order: FAILED \(failure)") }
        print("smoke-inspection-order: \(failures.isEmpty ? "ok" : "\(failures.count) failed")")
        exit(failures.isEmpty ? 0 : 1)
    }
    dispatchMain()
}

// `--smoke-import-result` checks how a file-importer result reaches the inspector:
// a cancelled picker shows nothing, a failure is shown, and a new selection clears it.
// No file is opened or read.
if CommandLine.arguments.contains("--smoke-import-result") {
    let ok = MainActor.assumeIsolated { () -> Bool in
        var failures: [String] = []
        let model = InspectionModel()
        model.importFinished(.failure(CocoaError(.userCancelled)))
        if model.selectionError != nil || model.fileName != nil { failures.append("cancel shows a message") }
        model.importFinished(.failure(CocoaError(.fileReadNoPermission)))
        if model.selectionError == nil || model.fileName != nil { failures.append("failure is not shown") }
        model.requestImport()
        if model.selectionError != nil || !model.importing { failures.append("reopening keeps the old message") }
        model.importFinished(.failure(CocoaError(.fileReadNoPermission)))
        model.inspect(URL(fileURLWithPath: "/nonexistent/import-result-check"))
        if model.selectionError != nil || model.fileName != "import-result-check" {
            failures.append("a new selection keeps the old message")
        }
        for failure in failures { print("smoke-import-result: FAILED \(failure)") }
        return failures.isEmpty
    }
    print("smoke-import-result: \(ok ? "ok" : "failed")")
    exit(ok ? 0 : 1)
}

// `--smoke-run-program <file> <exit status> <last stage or -> <stderr text> [-- <args>]` runs a
// file through the bundled worker with tracing on, as the Run button does with the
// HUD on, and checks what the app would show. It needs no GPU for a program run.
if let flag = CommandLine.arguments.firstIndex(of: "--smoke-run-program") {
    let values = CommandLine.arguments
    guard flag + 4 < values.count, let expectedExit = Int32(values[flag + 2]) else {
        print("smoke-run-program: usage: --smoke-run-program <file> <exit> <stage|-> <stderr text>")
        exit(2)
    }
    let file = URL(fileURLWithPath: values[flag + 1])
    let expectedStage = values[flag + 3] == "-" ? nil : values[flag + 3]
    let programArguments = values.firstIndex(of: "--").map { Array(values[($0 + 1)...]) } ?? []
    var failures: [String] = []
    let traced = ProgramRunner.run(file, trace: true, programArguments: programArguments)
    if traced.exitStatus != expectedExit { failures.append("exit \(String(describing: traced.exitStatus))") }
    if traced.lastStage != expectedStage { failures.append("stage \(traced.lastStage ?? "none")") }
    if !traced.standardError.contains(values[flag + 4]) { failures.append("stderr \(traced.standardError)") }
    // Without the HUD the same run is untraced.
    let plain = ProgramRunner.run(file, trace: false, programArguments: programArguments)
    if plain.exitStatus != expectedExit || plain.lastStage != nil { failures.append("untraced run differs") }
    // A missing runner is reported, not crashed on.
    let missing = ProgramRunner.run(file, trace: false, worker: nil)
    if missing.launchError == nil { failures.append("missing runner not reported") }
    for failure in failures { print("smoke-run-program: FAILED \(failure)") }
    print("smoke-run-program: \(traced.summary) \(failures.isEmpty ? "ok" : "failed")")
    exit(failures.isEmpty ? 0 : 1)
}

// `--smoke-hud` checks that the Metal HUD setting is off by default, persists, and
// turns tracing on for program runs. It needs no GPU and opens no window.
if CommandLine.arguments.contains("--smoke-hud") {
    let ok = MainActor.assumeIsolated { () -> Bool in
        let suite = "local.xenora.smoke-hud.\(ProcessInfo.processInfo.processIdentifier)"
        guard let defaults = UserDefaults(suiteName: suite) else { return false }
        defer { defaults.removePersistentDomain(forName: suite) }
        var failures: [String] = []
        let first = HudSettings(defaults: defaults)
        if first.enabled { failures.append("on by default") }
        first.enabled = true
        if !HudSettings(defaults: defaults).enabled { failures.append("on does not persist") }
        first.enabled = false
        if HudSettings(defaults: defaults).enabled { failures.append("off does not persist") }
        let file = URL(fileURLWithPath: "/nonexistent/program.exe")
        let on = ProgramRunner.arguments(for: file, trace: true, programArguments: ["a b", "c"])
        if on != ["run", "--experimental", "--trace", "--pe", file.path, "--", "a b", "c"] {
            failures.append("traced arguments \(on)")
        }
        let off = ProgramRunner.arguments(for: file, trace: false, programArguments: [])
        if off != ["run", "--experimental", "--pe", file.path] { failures.append("plain arguments \(off)") }
        if ProgramRunner.lastStage(in: "x\ntrace stage=mapped base=1\ntrace stage=relocated n=0\nrun failed") != "relocated" {
            failures.append("stage parsing")
        }
        for failure in failures { print("smoke-hud: FAILED \(failure)") }
        return failures.isEmpty
    }
    print("smoke-hud: \(ok ? "ok" : "failed")")
    exit(ok ? 0 : 1)
}

// Keep injected status failures separate from the actual Metal fixtures.
if let check = CommandLine.arguments.first(where: {
    $0 == "--smoke-loop-lifecycle" || $0 == "--smoke-native-loop-lifecycle"
}) {
    Task { @MainActor in
        let failures = check == "--smoke-loop-lifecycle"
            ? await checkFrameLoopFailures()
            : await checkNativeFrameLoop()
        for failure in failures { print("\(check): FAILED \(failure)") }
        print("\(check): \(failures.isEmpty ? "ok" : "failed")")
        exit(failures.isEmpty ? 0 : 1)
    }
    dispatchMain()
}

// `--smoke` prints the probe and exits without opening a window, for scripts and CI.
if CommandLine.arguments.contains("--smoke") {
    let probe = RuntimeProbe.run()
    print(probe.summary)
    exit(probe.connected ? 0 : 1)
}
XenoraApp.main()
