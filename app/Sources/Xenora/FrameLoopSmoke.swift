// SPDX-License-Identifier: GPL-3.0-or-later
import CoreGraphics
import CXenora
import Foundation

/// Scripted failure paths. This never opens a Metal device.
private final class ScriptedFrameLoop: FrameLoopAPI, @unchecked Sendable {
    enum Operation: String {
        case create, destroy, standard, interactive, sendInput, pause, resume, stop, status
    }

    // Deinitialization can call the API from outside the main actor.
    private let lock = NSLock()
    private var live: xenora_session = 0
    private var next: xenora_session = 1
    private var codes: [Operation: Int32] = [:]
    private var calls: [Operation] = []
    private var invalidCalls = 0
    private var snapshot = (state: UInt32(XENORA_LOOP_FINISHED), completed: UInt32(1), frame: UInt32(0), pixel: UInt8(0))

    func report(_ state: Int, completed: UInt32, frame: UInt32, pixel: UInt8) {
        lock.withLock { snapshot = (UInt32(state), completed, frame, pixel) }
    }

    func fail(_ operation: Operation, _ code: Int32) {
        lock.withLock { codes[operation] = code }
    }

    func count(_ operation: Operation) -> Int {
        lock.withLock { calls.filter { $0 == operation }.count }
    }

    var clean: Bool { lock.withLock { live == 0 && invalidCalls == 0 } }

    private func call(_ operation: Operation, _ handle: xenora_session) -> Int32 {
        calls.append(operation)
        guard handle == live else {
            invalidCalls += 1
            return Int32(XENORA_INVALID_HANDLE)
        }
        return codes.removeValue(forKey: operation) ?? Int32(XENORA_OK)
    }

    func create(_ handle: inout xenora_session) -> Int32 {
        lock.withLock {
            let code = call(.create, 0)
            if code == XENORA_OK {
                live = next
                next += 1
                handle = live
            }
            return code
        }
    }

    func destroy(_ handle: xenora_session) -> Int32 {
        lock.withLock {
            let code = call(.destroy, handle)
            if code == XENORA_OK { live = 0 }
            return code
        }
    }

    func start(_ handle: xenora_session, mode: FrameLoopMode, frames: UInt32) -> Int32 {
        lock.withLock { call(mode == .interactive ? .interactive : .standard, handle) }
    }

    func sendInput(_ handle: xenora_session, _ input: inout xenora_controller_input) -> Int32 {
        lock.withLock { call(.sendInput, handle) }
    }

    func pause(_ handle: xenora_session) -> Int32 { lock.withLock { call(.pause, handle) } }
    func resume(_ handle: xenora_session) -> Int32 { lock.withLock { call(.resume, handle) } }
    func stop(_ handle: xenora_session) -> Int32 { lock.withLock { call(.stop, handle) } }

    func status(
        _ handle: xenora_session, _ state: inout UInt32, _ completed: inout UInt32,
        _ frame: inout UInt32, _ rgba: UnsafeMutableBufferPointer<UInt8>
    ) -> Int32 {
        lock.withLock {
            let code = call(.status, handle)
            if code == XENORA_OK {
                state = snapshot.state
                completed = snapshot.completed
                frame = snapshot.frame
                if frame != XENORA_NO_FRAME {
                    for i in rgba.indices { rgba[i] = snapshot.pixel }
                }
            }
            return code
        }
    }
}

@MainActor
private func waitForLoop(_ body: () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(5)
    while !body() {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

@MainActor
func checkFrameLoopFailures() async -> [String] {
    var failures: [String] = []
    func expect(_ condition: Bool, _ message: String) {
        if !condition { failures.append(message) }
    }

    let api = ScriptedFrameLoop()
    let model = FrameLoopModel(api: api)
    func expectError(_ message: String, _ code: Int) {
        expect(model.errorMessage?.contains("\(message) (status \(code))") == true,
               "expected error for \(message), got \(model.errorMessage ?? "none")")
    }

    api.fail(.create, Int32(XENORA_LIMIT_EXCEEDED))
    model.start()
    expectError("create fixture session", Int(XENORA_LIMIT_EXCEEDED))
    expect(api.clean && api.count(.standard) == 0 && model.canStart, "create failure changed ownership")

    api.fail(.standard, Int32(XENORA_INVALID_ARGUMENT))
    api.fail(.destroy, Int32(XENORA_INTERNAL_ERROR))
    model.start()
    expectError("start frame loop", Int(XENORA_INVALID_ARGUMENT))
    expectError("destroy fixture session", Int(XENORA_INTERNAL_ERROR))
    expect(model.errorMessage?.hasPrefix("Failed to start") == true, "cleanup hid primary error")

    api.fail(.destroy, Int32(XENORA_INTERNAL_ERROR))
    model.startInteractive()
    expect(api.count(.create) == 2 && api.count(.interactive) == 0,
           "replacement created a second session after unsuccessful cleanup")
    expectError("destroy fixture session", Int(XENORA_INTERNAL_ERROR))

    model.startInteractive()
    expect(model.errorMessage == nil && model.isRunning && model.isInteractive,
           "retry did not destroy the failed session and start interactive mode")
    api.fail(.pause, Int32(XENORA_INVALID_STATE))
    model.pause()
    expectError("pause frame loop", Int(XENORA_INVALID_STATE))
    api.fail(.resume, Int32(XENORA_INVALID_HANDLE))
    model.resume()
    expectError("resume frame loop", Int(XENORA_INVALID_HANDLE))
    api.fail(.sendInput, Int32(XENORA_INVALID_ARGUMENT))
    model.sendInput(buttons: UInt32(XENORA_INPUT_LEFT))
    expectError("send controller input", Int(XENORA_INVALID_ARGUMENT))
    api.fail(.stop, Int32(XENORA_INTERNAL_ERROR))
    model.stop()
    expectError("stop frame loop", Int(XENORA_INTERNAL_ERROR))

    model.stop()
    expect(await waitForLoop { model.canStart } && !model.isRunning, "successful stop did not allow restart")
    let priorPolls = api.count(.status)
    model.startInteractive()
    model.start()
    expect(model.errorMessage == nil && model.image == nil && model.isRunning && !model.isInteractive,
           "mode switch did not clear the previous presentation")
    expect(await waitForLoop { model.image != nil }, "replacement did not publish a frame")
    expect(api.count(.status) == priorPolls + 1 && model.completed == 1 && model.frameId == 0,
           "obsolete poll continued after replacement")

    api.fail(.status, Int32(XENORA_EXECUTION_FAILED))
    model.startInteractive()
    expect(await waitForLoop { model.canStart }, "status failure did not end polling")
    expectError("read frame loop status", Int(XENORA_EXECUTION_FAILED))
    expect(api.clean, "status failure did not retire the live session")

    let teardownAPI = ScriptedFrameLoop()
    var closing: FrameLoopModel? = FrameLoopModel(api: teardownAPI)
    closing?.startInteractive()
    weak let weakModel = closing
    closing = nil
    expect(weakModel == nil && teardownAPI.clean && teardownAPI.count(.destroy) == 1,
           "teardown leaked its model or session")
    try? await Task.sleep(for: .milliseconds(35))
    expect(teardownAPI.count(.status) == 0, "poll ran after teardown")
    return failures + (await checkFramePolling())
}

@MainActor
private final class FramePollingProbe {
    var intervals: [Duration] = []
    var controller: DetectedController?
}

@MainActor
private func checkFramePolling() async -> [String] {
    var failures: [String] = []
    func expect(_ condition: Bool, _ message: String) {
        if !condition { failures.append(message) }
    }

    let api = ScriptedFrameLoop()
    let probe = FramePollingProbe()
    let model = FrameLoopModel(
        api: api,
        readController: { probe.controller },
        sleepBeforePoll: { interval in
            probe.intervals.append(interval)
            // Keep the heartbeat pending until a control command cancels it.
            try await Task.sleep(for: .seconds(60))
        }
    )
    let controller = DetectedController(
        identity: ObjectIdentifier(api), name: "Smoke controller", supportsHaptics: false,
        snapshot: ControllerSnapshot(buttons: UInt32(XENORA_INPUT_LEFT), x: 0, y: 0, leftTrigger: 0, rightTrigger: 0)
    )
    func pixels(_ image: CGImage?) -> Data? {
        image?.dataProvider?.data.map { $0 as Data }
    }

    model.startInteractive()
    expect(await waitForLoop { probe.intervals.count == 1 } && probe.intervals.last == .milliseconds(16),
           "running loop did not schedule the running heartbeat")

    probe.controller = controller
    api.report(XENORA_LOOP_PAUSED, completed: 1, frame: 0, pixel: 0x22)
    model.pause()
    let first = model.image
    expect(model.isPaused && first != nil && model.completed == 1 && model.controller == controller
           && api.count(.sendInput) == 1 && api.count(.status) == 1,
           "pause did not immediately refresh the frame and controller")
    expect(await waitForLoop { probe.intervals.count == 2 } && probe.intervals.last == .milliseconds(250),
           "paused loop did not replace the running heartbeat")

    api.report(XENORA_LOOP_RUNNING, completed: 2, frame: 0, pixel: 0x22)
    model.resume()
    expect(model.isRunning && model.image === first && model.completed == 2
           && api.count(.sendInput) == 1 && api.count(.status) == 2,
           "resume republished the same frame or failed to refresh immediately")
    expect(await waitForLoop { probe.intervals.count == 3 } && probe.intervals.last == .milliseconds(16),
           "resumed loop did not replace the paused heartbeat")

    probe.controller = nil
    api.report(XENORA_LOOP_PAUSED, completed: 3, frame: 1, pixel: 0x77)
    model.pause()
    let second = model.image
    let byteCount = Int(XENORA_FIXTURE_FRAME_WIDTH * XENORA_FIXTURE_FRAME_HEIGHT * 4)
    expect(model.isPaused && model.frameId == 1 && second !== first
           && pixels(second) == Data(repeating: 0x77, count: byteCount)
           && pixels(first) == Data(repeating: 0x22, count: byteCount),
           "new status did not publish a separate, immutable pixel snapshot")
    expect(model.controller == nil && api.count(.sendInput) == 2,
           "paused interactive loop did not report the controller disconnect")

    api.report(XENORA_LOOP_STOPPED, completed: 3, frame: 1, pixel: 0x77)
    model.stop()
    expect(model.state == XENORA_LOOP_STOPPED && model.canStart && model.image === second
           && api.count(.status) == 4,
           "stop did not immediately publish terminal status and retain the last frame")
    return failures
}

@MainActor
func checkNativeFrameLoop() async -> [String] {
    var failures: [String] = []
    let model = FrameLoopModel(readController: { nil })
    func expectedPixels(redColumn: Int) -> Data {
        let row = (0..<4).flatMap { column -> [UInt8] in
            column == redColumn ? [255, 0, 0, 255] : [19, 43, 93, 251]
        }
        return Data(row + row + row)
    }
    func pixels() -> Data? {
        guard let data = model.image?.dataProvider?.data else { return nil }
        return data as Data
    }

    // Start then pause promptly with a long enough fixture that it cannot
    // finish before the pause.
    model.start(frameCount: 10_000)
    model.pause()
    if await waitForLoop({ model.isPaused }) {
        model.resume()
        if !(await waitForLoop({ model.isRunning && model.image != nil })) {
            failures.append("resumed standard fixture did not publish a frame")
        }
        model.stop()
        if !(await waitForLoop({ model.state == XENORA_LOOP_STOPPED })) {
            failures.append("resumed standard fixture did not stop")
        }
    } else {
        failures.append("standard fixture did not pause")
    }
    if let error = model.errorMessage { failures.append(error) }

    model.start(frameCount: 4)
    if !(await waitForLoop({ model.canStart })) || model.completed != 4
        || model.frameId != 3 || pixels() != expectedPixels(redColumn: 3) || model.errorMessage != nil {
        failures.append("standard fixture restart did not finish with frame 3")
    }

    model.startInteractive(frameCount: 8)
    if !(await waitForLoop({ model.canStart })) || model.completed != 8
        || model.frameId != 0 || pixels() != expectedPixels(redColumn: 0) || !model.isInteractive
        || model.errorMessage != nil {
        failures.append("interactive fixture did not finish with eight frames at column 0")
    }
    return failures
}
