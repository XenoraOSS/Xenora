// SPDX-License-Identifier: GPL-3.0-or-later
import CoreGraphics
import CXenora
import Foundation
import SwiftUI

enum FrameLoopMode {
    case standard
    case interactive

    var startOperation: String {
        switch self {
        case .standard: "start frame loop"
        case .interactive: "start interactive loop"
        }
    }
}

protocol FrameLoopAPI: Sendable {
    func create(_ handle: inout xenora_session) -> Int32
    func destroy(_ handle: xenora_session) -> Int32
    func start(_ handle: xenora_session, mode: FrameLoopMode, frames: UInt32) -> Int32
    func sendInput(_ handle: xenora_session, _ input: inout xenora_controller_input) -> Int32
    func pause(_ handle: xenora_session) -> Int32
    func resume(_ handle: xenora_session) -> Int32
    func stop(_ handle: xenora_session) -> Int32
    func journalSize(_ handle: xenora_session, required: inout UInt64) -> Int32
    func status(
        _ handle: xenora_session, _ state: inout UInt32, _ completed: inout UInt32,
        _ frame: inout UInt32, _ rgba: UnsafeMutableBufferPointer<UInt8>
    ) -> Int32
}

struct NativeFrameLoopAPI: FrameLoopAPI {
    func create(_ handle: inout xenora_session) -> Int32 {
        xenora_session_create(XENORA_ABI_VERSION, &handle)
    }

    func destroy(_ handle: xenora_session) -> Int32 {
        xenora_session_destroy(handle)
    }

    func start(_ handle: xenora_session, mode: FrameLoopMode, frames: UInt32) -> Int32 {
        switch mode {
        case .standard: xenora_session_start_frame_loop(handle, frames)
        case .interactive: xenora_session_start_interactive_loop(handle, frames)
        }
    }

    func sendInput(_ handle: xenora_session, _ input: inout xenora_controller_input) -> Int32 {
        xenora_session_send_controller_input(handle, &input)
    }

    func pause(_ handle: xenora_session) -> Int32 { xenora_session_pause_frame_loop(handle) }
    func resume(_ handle: xenora_session) -> Int32 { xenora_session_resume_frame_loop(handle) }
    func stop(_ handle: xenora_session) -> Int32 { xenora_session_stop_frame_loop(handle) }

    func journalSize(_ handle: xenora_session, required: inout UInt64) -> Int32 {
        xenora_session_frame_loop_journal(handle, nil, 0, &required)
    }

    func status(
        _ handle: xenora_session, _ state: inout UInt32, _ completed: inout UInt32,
        _ frame: inout UInt32, _ rgba: UnsafeMutableBufferPointer<UInt8>
    ) -> Int32 {
        xenora_session_frame_loop_status(handle, &state, &completed, &frame, rgba.baseAddress, UInt64(rgba.count))
    }
}

/// Persistent guest rendering model (Gate 3, #266; Gate 4, #276).
@MainActor
final class FrameLoopModel: ObservableObject {
    @Published var state: UInt32 = UInt32(XENORA_LOOP_NONE)
    @Published var completed: UInt32 = 0
    @Published var frameId: UInt32 = XENORA_NO_FRAME
    @Published var image: CGImage?
    @Published var errorMessage: String?
    @Published var isInteractive: Bool = false
    /// Completed frames per second over the last half second, measured on the host.
    @Published var hostFps: Double?
    @Published private(set) var controller: DetectedController?
    @Published private(set) var isStopping = false
    @Published private(set) var journalRecordCount: UInt64?

    private let api: any FrameLoopAPI
    private let readController: @MainActor () -> DetectedController?
    private let sleepBeforePoll: (@MainActor (Duration) async throws -> Void)?
    private static let horizontalButtons = UInt32(XENORA_INPUT_LEFT | XENORA_INPUT_RIGHT)
    private var screenDirection: UInt32 = 0
    private var lastSentInput = xenora_controller_input()
    private var controllerInputError: String?
    private var session: xenora_session = 0
    private var pollTask: Task<Void, Never>?
    private var rgba = [UInt8](repeating: 0, count: Int(XENORA_FIXTURE_FRAME_WIDTH * XENORA_FIXTURE_FRAME_HEIGHT * 4))
    private var rateSample: (time: ContinuousClock.Instant, completed: UInt32)?

    init(
        api: any FrameLoopAPI = NativeFrameLoopAPI(),
        readController: @escaping @MainActor () -> DetectedController? = pollGameController,
        sleepBeforePoll: (@MainActor (Duration) async throws -> Void)? = nil
    ) {
        self.api = api
        self.readController = readController
        self.sleepBeforePoll = sleepBeforePoll
    }

    deinit {
        pollTask?.cancel()
        if session != 0 {
            _ = api.destroy(session)
        }
    }

    var isRunning: Bool { !isStopping && state == UInt32(XENORA_LOOP_RUNNING) }
    var isPaused: Bool { !isStopping && state == UInt32(XENORA_LOOP_PAUSED) }
    var canStart: Bool {
        guard !isStopping else { return false }
        return session == 0 || state == UInt32(XENORA_LOOP_NONE) || state == UInt32(XENORA_LOOP_FINISHED)
            || state == UInt32(XENORA_LOOP_STOPPED) || state == UInt32(XENORA_LOOP_FAILED)
    }

    var stateDescription: String {
        if isStopping { return "Stopping…" }
        let summary: String
        if let count = journalRecordCount {
            summary = " • \(count) verified journal \(count == 1 ? "entry" : "entries")"
        } else {
            summary = ""
        }
        return switch state {
        case UInt32(XENORA_LOOP_NONE): "Idle"
        case UInt32(XENORA_LOOP_RUNNING): "Running"
        case UInt32(XENORA_LOOP_PAUSED): "Paused"
        case UInt32(XENORA_LOOP_FINISHED): "Finished" + summary
        case UInt32(XENORA_LOOP_STOPPED): "Stopped" + summary
        case UInt32(XENORA_LOOP_FAILED): "Failed"
        default: "Unknown (\(state))"
        }
    }

    func start(frameCount: UInt32 = 300) {
        start(.standard, frameCount: frameCount)
    }

    func startInteractive(frameCount: UInt32 = 300) {
        start(.interactive, frameCount: frameCount)
    }

    private func start(_ mode: FrameLoopMode, frameCount: UInt32) {
        guard !isStopping else { return }
        errorMessage = nil
        cancelPolling()
        journalRecordCount = nil
        state = UInt32(XENORA_LOOP_FAILED)
        isInteractive = false
        controller = nil
        screenDirection = 0
        lastSentInput = xenora_controller_input()
        controllerInputError = nil
        completed = 0
        frameId = XENORA_NO_FRAME
        image = nil
        hostFps = nil
        rateSample = nil

        guard destroySession() else { return }
        var newSession: xenora_session = 0
        let created = api.create(&newSession)
        guard check(created, operation: "create fixture session") else { return }

        session = newSession
        guard check(api.start(session, mode: mode, frames: frameCount), operation: mode.startOperation) else {
            _ = destroySession()
            return
        }
        isInteractive = mode == .interactive
        state = UInt32(XENORA_LOOP_RUNNING)
        startPolling(session)
    }

    private func cancelPolling() {
        pollTask?.cancel()
        pollTask = nil
    }

    // Keep the handle for another cleanup attempt if destruction failed and
    // the native session might still exist.
    private func destroySession() -> Bool {
        guard session != 0 else { return true }
        let code = api.destroy(session)
        if code == XENORA_OK || code == XENORA_INVALID_HANDLE {
            session = 0
        }
        return check(code, operation: "destroy fixture session", append: true)
    }

    @discardableResult
    private func check(_ code: Int32, operation: String, append: Bool = false) -> Bool {
        guard code != XENORA_OK else { return true }
        let message = "Failed to \(operation) (status \(code))."
        if append, let previous = errorMessage {
            errorMessage = previous + " " + message
        } else {
            errorMessage = message
        }
        return false
    }

    func sendInput(buttons: UInt32) {
        guard session != 0, isInteractive, !isStopping else { return }
        screenDirection = buttons & Self.horizontalButtons
        updateGameController()
    }

    private func updateGameController() {
        let next = readController()
        if next != controller { controller = next }
        let snapshot = next?.snapshot
        let physicalButtons = snapshot?.buttons ?? 0
        // The screen replaces only Left and Right until Release.
        let buttons = screenDirection == 0
            ? physicalButtons : (physicalButtons & ~Self.horizontalButtons) | screenDirection
        let state = xenora_input_state(
            buttons: buttons,
            stick_x: snapshot?.x ?? 0,
            stick_y: snapshot?.y ?? 0,
            mouse_dx: 0,
            mouse_dy: 0,
            mouse_buttons: 0,
            connected: next != nil || screenDirection != 0 ? 1 : 0,
            reserved: 0
        )
        var input = xenora_controller_input(
            state: state,
            left_trigger: UInt16(((snapshot?.leftTrigger ?? 0) * 65535).rounded()),
            right_trigger: UInt16(((snapshot?.rightTrigger ?? 0) * 65535).rounded())
        )
        guard state.buttons != lastSentInput.state.buttons
            || state.stick_x != lastSentInput.state.stick_x
            || state.stick_y != lastSentInput.state.stick_y
            || state.connected != lastSentInput.state.connected
            || input.left_trigger != lastSentInput.left_trigger
            || input.right_trigger != lastSentInput.right_trigger else {
            clearControllerInputError()
            return
        }

        let code = api.sendInput(session, &input)
        if code == XENORA_OK {
            lastSentInput = input
            clearControllerInputError()
            return
        }
        let message = "Failed to send controller input (status \(code))."
        // A retry must not replace a later command error or republish the same message.
        let canShow = controllerInputError == nil || errorMessage == controllerInputError
        controllerInputError = message
        if canShow && errorMessage != message { errorMessage = message }
    }

    private func clearControllerInputError() {
        guard let previous = controllerInputError else { return }
        controllerInputError = nil
        if errorMessage == previous { errorMessage = nil }
    }

    func pause() {
        guard session != 0, !isStopping,
              check(api.pause(session), operation: "pause frame loop") else { return }
        refreshPolling()
    }

    func resume() {
        guard session != 0, !isStopping,
              check(api.resume(session), operation: "resume frame loop") else { return }
        refreshPolling()
    }

    @discardableResult
    func stop() -> Task<Void, Never>? {
        guard session != 0, !isStopping else { return nil }
        let handle = session
        let api = self.api
        let wasPolling = pollTask != nil
        isStopping = true
        cancelPolling()

        // Stop joins the native worker. The task keeps the session alive
        // through the join, and the dispatch queue can block without
        // occupying the main actor or a Swift concurrency executor thread.
        return Task {
            let code: Int32 = await withCheckedContinuation { continuation in
                DispatchQueue.global(qos: .userInitiated).async {
                    continuation.resume(returning: api.stop(handle))
                }
            }
            let shouldPoll: Bool
            if check(code, operation: "stop frame loop") {
                shouldPoll = poll()
            } else {
                shouldPoll = wasPolling
            }
            isStopping = false
            if shouldPoll { startPolling(handle) }
        }
    }

    // Refresh native state after a successful control command, then replace the
    // pending sleep so resume never waits for the next paused heartbeat.
    private func refreshPolling() {
        guard pollTask != nil else { return }
        cancelPolling()
        if poll() { startPolling(session) }
    }

    private func startPolling(_ handle: xenora_session) {
        var interval = pollInterval
        pollTask = Task { [weak self, sleepBeforePoll] in
            while !Task.isCancelled {
                if let sleepBeforePoll {
                    try? await sleepBeforePoll(interval)
                } else {
                    try? await Task.sleep(for: interval)
                }
                // Cancellation can arrive during sleep, including on restart
                // with a replacement handle or a new cadence for the same one.
                guard !Task.isCancelled, let self, self.session == handle, self.poll() else { return }
                interval = self.pollInterval
            }
        }
    }

    private var pollInterval: Duration {
        isPaused ? .milliseconds(250) : .milliseconds(16)
    }

    /// Returns whether the native loop still needs a heartbeat.
    private func poll() -> Bool {
        var st: UInt32 = 0
        var comp: UInt32 = 0
        var fId: UInt32 = XENORA_NO_FRAME
        let code = rgba.withUnsafeMutableBufferPointer { buffer in
            api.status(session, &st, &comp, &fId, buffer)
        }
        guard check(code, operation: "read frame loop status") else {
            journalRecordCount = nil
            state = UInt32(XENORA_LOOP_FAILED)
            cancelPolling()
            _ = destroySession()
            return false
        }

        let active = st == XENORA_LOOP_RUNNING || st == XENORA_LOOP_PAUSED
        if isInteractive && active { updateGameController() }
        if state != st {
            if st == XENORA_LOOP_FINISHED || st == XENORA_LOOP_STOPPED {
                readJournalSize(completed: comp)
            } else if journalRecordCount != nil {
                journalRecordCount = nil
            }
            state = st
        }
        if completed != comp { completed = comp }
        // Interactive frame IDs identify fixture columns, whose pixels repeat.
        if frameId != fId {
            if fId == XENORA_NO_FRAME {
                frameId = fId
                image = nil
            } else if let snapshot = makeImage() {
                frameId = fId
                image = snapshot
            }
        }

        let now = ContinuousClock.now
        if let sample = rateSample {
            let elapsed = sample.time.duration(to: now)
            if elapsed >= .milliseconds(500) || (!active && comp > sample.completed && elapsed > .zero) {
                let parts = elapsed.components
                let seconds = Double(parts.seconds) + Double(parts.attoseconds) / 1e18
                let rate = Double(comp &- sample.completed) / seconds
                if hostFps != rate { hostFps = rate }
                rateSample = (now, comp)
            }
        } else {
            rateSample = (now, comp)
        }

        if !active {
            if st == XENORA_LOOP_FAILED && errorMessage == nil {
                errorMessage = "Fixture frame loop entered the failed state."
            }
            cancelPolling()
        }
        return active
    }

    private func readJournalSize(completed: UInt32) {
        var required: UInt64 = 0
        let code = api.journalSize(session, required: &required)
        let expectedCode = required == 0 ? XENORA_OK : XENORA_BUFFER_TOO_SMALL
        guard code == expectedCode else {
            if code == XENORA_OK || code == XENORA_BUFFER_TOO_SMALL {
                errorMessage = "The guest journal returned an invalid size response."
            } else {
                check(code, operation: "read guest journal size", append: true)
            }
            return
        }
        guard required == UInt64(completed) && required <= UInt64(XENORA_FRAME_LOOP_MAX_FRAMES) else {
            errorMessage = "The guest journal entry count does not match the completed frames."
            return
        }
        journalRecordCount = required
    }

    private func makeImage() -> CGImage? {
        // The provider must own a snapshot because subsequent polls overwrite rgba.
        let pixels = rgba.withUnsafeBytes { Data($0) }
        guard let provider = CGDataProvider(data: pixels as CFData) else { return nil }
        return CGImage(
            width: Int(XENORA_FIXTURE_FRAME_WIDTH),
            height: Int(XENORA_FIXTURE_FRAME_HEIGHT),
            bitsPerComponent: 8,
            bitsPerPixel: 32,
            bytesPerRow: Int(XENORA_FIXTURE_FRAME_WIDTH) * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedLast.rawValue),
            provider: provider,
            decode: nil,
            shouldInterpolate: false,
            intent: .defaultIntent
        )
    }
}
