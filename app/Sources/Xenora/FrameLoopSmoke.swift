// SPDX-License-Identifier: GPL-3.0-or-later
import Combine
import CoreGraphics
import CXenora
import Foundation
import GameController

private final class StopGate: Sendable {
    private let release = DispatchSemaphore(value: 0)

    func wait() -> Bool { release.wait(timeout: .now() + .seconds(3)) == .success }
    func open() { release.signal() }
}

/// Scripted failure paths. This never opens a Metal device.
private final class ScriptedFrameLoop: FrameLoopAPI, @unchecked Sendable {
    enum Operation: String {
        case create, destroy, standard, interactive, sendInput, sendXInput, pollXInputVibration, pause, resume, stop, status, journalSize
    }

    // Deinitialization can call the API from outside the main actor.
    private let lock = NSLock()
    private var live: xenora_session = 0
    private var next: xenora_session = 1
    private var codes: [Operation: Int32] = [:]
    private var persistentCodes: [Operation: Int32] = [:]
    private var calls: [Operation] = []
    private var sentInputs: [(handle: xenora_session, input: xenora_controller_input)] = []
    private var sentXInputs: [(handle: xenora_session, userIndex: UInt32, input: xenora_xinput_state)] = []
    private var sentXInputBatteries: [(userIndex: UInt32, battery: xenora_xinput_battery)] = []
    private var vibrations = [xenora_xinput_vibration](repeating: xenora_xinput_vibration(), count: 4)
    private var vibrationPolls: [UInt32] = []
    private var invalidCalls = 0
    private var nextStopGate: StopGate?
    private var snapshot = (state: UInt32(XENORA_LOOP_FINISHED), completed: UInt32(1), frame: UInt32(0), pixel: UInt8(0))

    func report(_ state: Int, completed: UInt32, frame: UInt32, pixel: UInt8) {
        lock.withLock { snapshot = (UInt32(state), completed, frame, pixel) }
    }

    func fail(_ operation: Operation, _ code: Int32) {
        lock.withLock { codes[operation] = code }
    }

    func failContinuously(_ operation: Operation, _ code: Int32) {
        lock.withLock { persistentCodes[operation] = code }
    }

    func count(_ operation: Operation) -> Int {
        lock.withLock { calls.filter { $0 == operation }.count }
    }

    func blockNextStop() -> StopGate {
        let gate = StopGate()
        lock.withLock { nextStopGate = gate }
        return gate
    }

    var inputs: [xenora_controller_input] { lock.withLock { sentInputs.map(\.input) } }

    var xinputs: [(userIndex: UInt32, input: xenora_xinput_state)] {
        lock.withLock { sentXInputs.map { ($0.userIndex, $0.input) } }
    }

    var xinputBatteries: [(userIndex: UInt32, battery: xenora_xinput_battery)] {
        lock.withLock { sentXInputBatteries }
    }

    var polledVibrationSlots: [UInt32] { lock.withLock { vibrationPolls } }

    func reportVibration(
        sequence: UInt32, left: UInt16, right: UInt16, connected: Bool, enabled: Bool = true,
        slot: Int? = nil
    ) {
        lock.withLock {
            let value = xenora_xinput_vibration(
                sequence: sequence,
                left_motor: left,
                right_motor: right,
                connected: connected ? 1 : 0,
                enabled: enabled ? 1 : 0,
                reserved: (0, 0)
            )
            if let slot, vibrations.indices.contains(slot) {
                vibrations[slot] = value
            } else {
                vibrations = [xenora_xinput_vibration](repeating: value, count: 4)
            }
        }
    }

    var lastInputHandle: xenora_session? { lock.withLock { sentInputs.last?.handle } }

    var clean: Bool { lock.withLock { live == 0 && invalidCalls == 0 } }

    private func call(_ operation: Operation, _ handle: xenora_session) -> Int32 {
        calls.append(operation)
        guard handle == live else {
            invalidCalls += 1
            return Int32(XENORA_INVALID_HANDLE)
        }
        return persistentCodes[operation] ?? codes.removeValue(forKey: operation) ?? Int32(XENORA_OK)
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
        lock.withLock {
            sentInputs.append((handle, input))
            return call(.sendInput, handle)
        }
    }

    func sendXInput(_ handle: xenora_session, _ userIndex: UInt32, _ input: inout xenora_xinput_state) -> Int32 {
        lock.withLock {
            sentXInputs.append((handle, userIndex, input))
            return call(.sendXInput, handle)
        }
    }

    func sendXInputWithBattery(
        _ handle: xenora_session,
        _ userIndex: UInt32,
        _ input: inout xenora_xinput_state,
        _ battery: inout xenora_xinput_battery
    ) -> Int32 {
        lock.withLock {
            sentXInputs.append((handle, userIndex, input))
            sentXInputBatteries.append((userIndex, battery))
            return call(.sendXInput, handle)
        }
    }

    func pollXInputVibration(
        _ handle: xenora_session, _ userIndex: UInt32, _ output: inout xenora_xinput_vibration
    ) -> Int32 {
        lock.withLock {
            vibrationPolls.append(userIndex)
            let code = call(.pollXInputVibration, handle)
            if code == XENORA_OK, vibrations.indices.contains(Int(userIndex)) {
                output = vibrations[Int(userIndex)]
            }
            return code
        }
    }

    func pause(_ handle: xenora_session) -> Int32 { lock.withLock { call(.pause, handle) } }
    func resume(_ handle: xenora_session) -> Int32 { lock.withLock { call(.resume, handle) } }
    func stop(_ handle: xenora_session) -> Int32 {
        let (code, gate) = lock.withLock {
            let gate = nextStopGate
            nextStopGate = nil
            return (call(.stop, handle), gate)
        }
        if let gate, !gate.wait() { return Int32(XENORA_INTERNAL_ERROR) }
        return code
    }

    func journalSize(_ handle: xenora_session, required: inout UInt64) -> Int32 {
        lock.withLock {
            let code = call(.journalSize, handle)
            guard code == XENORA_OK else { return code }
            guard snapshot.state == XENORA_LOOP_FINISHED || snapshot.state == XENORA_LOOP_STOPPED else {
                required = 0
                return Int32(XENORA_INVALID_STATE)
            }
            required = UInt64(snapshot.completed)
            return required == 0 ? Int32(XENORA_OK) : Int32(XENORA_BUFFER_TOO_SMALL)
        }
    }

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
    await model.stop()?.value
    expectError("stop frame loop", Int(XENORA_INTERNAL_ERROR))

    await model.stop()?.value
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
    return failures + (await checkFramePolling()) + (await checkNonblockingStop())
        + (await checkFailedStopRecovery())
        + checkControllerComposition() + checkMultiplayerSlots() + checkControllerHandoffs()
        + (await checkControllerErrorRecovery()) + (await checkVibrationOutput())
        + (await checkMultiSlotVibration())
}

@MainActor
private func checkNonblockingStop() async -> [String] {
    var failures: [String] = []
    func expect(_ condition: Bool, _ message: String) {
        if !condition { failures.append(message) }
    }

    let api = ScriptedFrameLoop()
    let model = FrameLoopModel(
        api: api, readController: { nil },
        sleepBeforePoll: { _ in try await Task.sleep(for: .milliseconds(5)) }
    )
    model.startInteractive()
    api.report(XENORA_LOOP_PAUSED, completed: 1, frame: 0, pixel: 0x22)
    model.pause()
    let pausedImage = model.image
    expect(pausedImage != nil, "paused loop did not publish its frame")
    let priorPolls = api.count(.status)
    let gate = api.blockNextStop()
    defer { gate.open() }
    guard let stop = model.stop() else { return ["paused loop did not start a stop task"] }
    expect(model.isStopping && !model.canStart && !model.isRunning && !model.isPaused,
           "stop did not disable controls before the worker join")
    expect(await waitForLoop { api.count(.stop) == 1 }, "worker did not enter the gated stop")

    let duplicate = model.stop()
    expect(duplicate == nil, "a duplicate stop started a second task")
    model.start()
    model.startInteractive()
    model.pause()
    model.resume()
    model.sendInput(buttons: UInt32(XENORA_INPUT_RIGHT))
    // A heartbeat has time to run here if the stop forgot to cancel it.
    try? await Task.sleep(for: .milliseconds(25))
    expect(model.isStopping && model.completed == 1 && model.frameId == 0
           && model.image === pausedImage,
           "main actor could not observe the last frame with the stop still blocked")
    expect(api.count(.status) == priorPolls && api.count(.stop) == 1 && api.count(.destroy) == 0
           && api.count(.create) == 1 && api.count(.standard) == 0 && api.count(.interactive) == 1
           && api.count(.pause) == 1 && api.count(.resume) == 0 && api.count(.sendInput) == 0,
           "a second native operation reached the handle while stop was blocked")

    api.report(XENORA_LOOP_STOPPED, completed: 2, frame: 1, pixel: 0x77)
    gate.open()
    await stop.value
    await duplicate?.value
    let count = Int(XENORA_FIXTURE_FRAME_WIDTH * XENORA_FIXTURE_FRAME_HEIGHT * 4)
    let finalPixels = model.image?.dataProvider?.data.map { $0 as Data }
    expect(!model.isStopping && model.canStart && model.state == XENORA_LOOP_STOPPED
           && model.completed == 2 && model.frameId == 1 && model.errorMessage == nil
           && finalPixels == Data(repeating: 0x77, count: count) && api.count(.status) == priorPolls + 1,
           "joined stop did not publish its final frame and terminal status")

    model.start()
    expect(model.isRunning && !model.isInteractive && model.image == nil
           && api.count(.create) == 2 && api.count(.destroy) == 1 && api.count(.standard) == 1,
           "completed stop did not allow a clean restart")
    return failures
}

@MainActor
private func checkFailedStopRecovery() async -> [String] {
    var failures: [String] = []
    func expect(_ condition: Bool, _ message: String) {
        if !condition { failures.append(message) }
    }

    let api = ScriptedFrameLoop()
    let model = FrameLoopModel(
        api: api, readController: { nil },
        sleepBeforePoll: { _ in try await Task.sleep(for: .milliseconds(5)) }
    )
    model.startInteractive()
    api.report(XENORA_LOOP_RUNNING, completed: 1, frame: 0, pixel: 0x33)
    let priorPolls = api.count(.status)
    api.fail(.stop, Int32(XENORA_INTERNAL_ERROR))
    await model.stop()?.value
    expect(model.errorMessage?.contains("stop frame loop (status \(Int(XENORA_INTERNAL_ERROR)))") == true,
           "failed stop did not publish its error")
    expect(!model.isStopping, "failed stop left controls disabled")
    expect(await waitForLoop { api.count(.status) > priorPolls },
           "failed stop did not restart polling")

    api.report(XENORA_LOOP_STOPPED, completed: 2, frame: 1, pixel: 0x44)
    await model.stop()?.value
    expect(model.canStart && !model.isStopping,
           "failed-stop recovery did not allow a clean stop")
    return failures
}


@MainActor
private final class RecordingVibrationSink: ControllerVibrationSink {
    private(set) var commands: [XInputVibrationCommand] = []
    private(set) var resetCount = 0
    var lastError: String?

    func apply(command: XInputVibrationCommand, haptics: GCDeviceHaptics?) {
        commands.append(command)
    }

    func reset() {
        resetCount += 1
    }
}

@MainActor
private func checkVibrationOutput() async -> [String] {
    var failures: [String] = []
    func expect(_ condition: Bool, _ message: String) {
        if !condition { failures.append(message) }
    }

    let api = ScriptedFrameLoop()
    let sink = RecordingVibrationSink()
    let controller = DetectedController(
        identity: ObjectIdentifier(api), name: "Rumble controller", supportsHaptics: true,
        snapshot: ControllerSnapshot(buttons: 0, x: 0, y: 0, leftTrigger: 0, rightTrigger: 0)
    )
    let model = FrameLoopModel(
        api: api,
        readController: { controller },
        vibrationSink: sink,
        sleepBeforePoll: { _ in try await Task.sleep(for: .milliseconds(5)) }
    )
    api.report(XENORA_LOOP_RUNNING, completed: 1, frame: 0, pixel: 0)
    api.reportVibration(sequence: 7, left: 32768, right: 65535, connected: true)
    model.startInteractive()
    let initialResets = sink.resetCount
    expect(await waitForLoop { sink.commands.count == 1 },
           "vibration command did not reach the output sink")
    expect(sink.commands.last == XInputVibrationCommand(
        sequence: 7, leftMotor: 32768, rightMotor: 65535, connected: true
    ), "vibration command bytes were altered")

    try? await Task.sleep(for: .milliseconds(25))
    expect(sink.commands.count == 1, "unchanged vibration sequence was applied repeatedly")
    api.reportVibration(sequence: 8, left: 0, right: 1024, connected: true)
    expect(await waitForLoop { sink.commands.count == 2 },
           "updated vibration sequence did not reach the output sink")
    expect(sink.commands.last == XInputVibrationCommand(
        sequence: 8, leftMotor: 0, rightMotor: 1024, connected: true
    ), "updated vibration command bytes were altered")

    api.reportVibration(sequence: 8, left: 0, right: 1024, connected: true, enabled: false)
    expect(await waitForLoop { sink.commands.count == 3 },
           "disabled XInput reporting did not reach the output sink")
    expect(sink.commands.last?.enabled == false && sink.commands.last?.rightIntensity == 0,
           "disabled XInput reporting did not suppress actuation")
    api.reportVibration(sequence: 8, left: 0, right: 1024, connected: true, enabled: true)
    expect(await waitForLoop { sink.commands.count == 4 },
           "re-enabled XInput reporting did not replay the registered vibration")
    expect(sink.commands.last?.enabled == true && sink.commands.last?.rightIntensity != 0,
           "re-enabled XInput reporting did not restore the registered intensity")

    api.report(XENORA_LOOP_STOPPED, completed: 2, frame: 0, pixel: 0)
    await model.stop()?.value
    expect(sink.resetCount > initialResets, "terminal loop did not reset vibration output")
    expect(model.controllerVibrationError == nil,
           "successful scripted vibration polling published an error")

    api.failContinuously(.pollXInputVibration, Int32(XENORA_INTERNAL_ERROR))
    api.report(XENORA_LOOP_RUNNING, completed: 2, frame: 0, pixel: 0)
    model.startInteractive()
    expect(await waitForLoop { model.controllerVibrationError?.contains("poll controller vibration") == true },
           "vibration polling failure was hidden; polled slots \(api.polledVibrationSlots), error \(String(describing: model.controllerVibrationError))")
    return failures
}

@MainActor
private func checkMultiSlotVibration() async -> [String] {
    var failures: [String] = []
    func expect(_ condition: Bool, _ message: String) {
        if !condition { failures.append(message) }
    }

    final class Token: NSObject {}
    func controller(_ token: NSObject, _ name: String) -> DetectedController {
        DetectedController(
            identity: ObjectIdentifier(token), name: name, supportsHaptics: true,
            snapshot: ControllerSnapshot(buttons: 0, x: 0, y: 0, leftTrigger: 0, rightTrigger: 0)
        )
    }
    let firstToken = Token()
    let secondToken = Token()
    let first = controller(firstToken, "first rumble")
    let second = controller(secondToken, "second rumble")
    var detected = [first, second]
    var sinks: [RecordingVibrationSink] = []
    let api = ScriptedFrameLoop()
    let model = FrameLoopModel(
        api: api,
        readControllers: { detected },
        vibrationSinkFactory: {
            let sink = RecordingVibrationSink()
            sinks.append(sink)
            return sink
        },
        sleepBeforePoll: { _ in try await Task.sleep(for: .milliseconds(5)) }
    )
    api.reportVibration(sequence: 3, left: 1000, right: 2000, connected: true, slot: 0)
    api.reportVibration(sequence: 4, left: 3000, right: 4000, connected: true, slot: 1)
    api.report(XENORA_LOOP_RUNNING, completed: 1, frame: 0, pixel: 0)
    model.startInteractive()

    expect(await waitForLoop { sinks.count == 2 && sinks.allSatisfy { $0.commands.count == 1 } },
           "multi-slot vibration did not create independent outputs for slots zero and one")
    guard sinks.count == 2 else {
        return failures + ["missing multi-slot vibration sinks: \(sinks.count), controllers \(model.controllers.count), polls \(api.polledVibrationSlots)"]
    }
    expect(sinks[0].commands.last?.sequence == 3 && sinks[1].commands.last?.sequence == 4,
           "multi-slot vibration commands were not routed to their respective slots")
    expect(api.polledVibrationSlots.contains(0) && api.polledVibrationSlots.contains(1),
           "multi-slot vibration did not poll both occupied slots")

    detected = [first]
    api.reportVibration(sequence: 4, left: 0, right: 0, connected: false, slot: 1)
    expect(await waitForLoop { sinks[1].commands.count == 2 },
           "slot-one disconnect did not reach its independent output")
    expect(sinks[1].commands.last?.connected == false
           && sinks[1].commands.last?.leftMotor == 0 && sinks[1].commands.last?.rightMotor == 0,
           "slot-one disconnect did not stop with a neutral command")
    expect(sinks[0].commands.count == 1,
           "slot-one disconnect disturbed the unchanged slot-zero output")

    api.report(XENORA_LOOP_STOPPED, completed: 1, frame: 0, pixel: 0)
    await model.stop()?.value
    expect(sinks.allSatisfy { $0.resetCount > 0 }, "terminal loop did not reset every vibration slot")
    return failures
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
    expect(model.journalRecordCount == nil && api.count(.journalSize) == 0,
           "active frame loop queried a guest journal")

    api.report(XENORA_LOOP_STOPPED, completed: 3, frame: 1, pixel: 0x77)
    await model.stop()?.value
    expect(model.state == XENORA_LOOP_STOPPED && model.canStart && model.image === second
           && api.count(.status) == 4 && api.count(.journalSize) == 1
           && model.stateDescription == "Stopped • 3 verified journal entries",
           "joined stop did not publish terminal status and retain the last frame")
    model.startInteractive()
    expect(model.journalRecordCount == nil && model.stateDescription == "Running",
           "replacement retained a stale guest journal count")
    return failures
}

@MainActor
private func checkControllerComposition() -> [String] {
    var failures: [String] = []
    let left = UInt32(XENORA_INPUT_LEFT)
    let right = UInt32(XENORA_INPUT_RIGHT)
    let action = UInt32(XENORA_INPUT_ACTION_A)
    let api = ScriptedFrameLoop()
    let controller = DetectedController(
        identity: ObjectIdentifier(api), name: "Held controller", supportsHaptics: false,
        snapshot: ControllerSnapshot(
            buttons: left | action, x: 1, y: -1, leftTrigger: 0.25, rightTrigger: 0.75,
            xinput: XInputSnapshotValues(
                buttons: XInputButtonBits.dpadLeft | XInputButtonBits.a,
                leftTrigger: 64,
                rightTrigger: 191,
                leftX: 32767,
                leftY: -32768
            )
        )
    )
    let model = FrameLoopModel(api: api, readController: { controller })
    model.startInteractive()
    api.report(XENORA_LOOP_PAUSED, completed: 1, frame: 0, pixel: 0)
    model.pause()
    model.sendInput(buttons: right)
    model.sendInput(buttons: UInt32(XENORA_INPUT_NONE))
    model.pause()

    let inputs = api.inputs
    let expectedButtons = [left | action, right | action, left | action]
    let actualButtons = inputs.map { $0.state.buttons }
    if actualButtons != expectedButtons {
        failures.append("controller composition sent buttons \(actualButtons), expected \(expectedButtons)")
    }
    if !inputs.allSatisfy({ input in
        input.state.connected == 1 && input.state.stick_x == 32767 && input.state.stick_y == -32767
            && input.left_trigger == 16384 && input.right_trigger == 49151
    }) {
        failures.append("screen direction or release replaced the connected controller's analog input")
    }
    let xinputs = api.xinputs
    let xbuttons = xinputs.map(\.input.buttons)
    let expectedXButtons = [
        XInputButtonBits.dpadLeft | XInputButtonBits.a,
        XInputButtonBits.dpadRight | XInputButtonBits.a,
        XInputButtonBits.dpadLeft | XInputButtonBits.a,
    ]
    if xbuttons != expectedXButtons {
        failures.append("controller composition sent XInput buttons \(xbuttons), expected \(expectedXButtons)")
    }
    if !xinputs.allSatisfy({ item in
        item.userIndex == 0 && item.input.connected == 1 && item.input.left_x == 32767
            && item.input.left_y == -32768 && item.input.left_trigger == 64 && item.input.right_trigger == 191
    }) {
        failures.append("screen direction or release replaced the connected controller's XInput analog state")
    }
    return failures
}

@MainActor
private func checkMultiplayerSlots() -> [String] {
    var failures: [String] = []
    func expect(_ condition: Bool, _ message: String) {
        if !condition { failures.append(message) }
    }

    final class Token: NSObject {}
    let firstToken = Token()
    let secondToken = Token()
    let thirdToken = Token()
    func controller(
        _ token: NSObject,
        _ buttons: UInt16,
        supportsHaptics: Bool = false,
        battery: XInputBatteryValues = XInputBatteryValues()
    ) -> DetectedController {
        DetectedController(
            identity: ObjectIdentifier(token),
            name: "Controller \(buttons)",
            supportsHaptics: supportsHaptics,
            snapshot: ControllerSnapshot(
                buttons: 0, x: 0, y: 0, leftTrigger: 0, rightTrigger: 0,
                xinput: XInputSnapshotValues(buttons: buttons)
            ),
            battery: battery
        )
    }

    let firstBattery = XInputBatteryValues(
        present: true,
        state: UInt8(XENORA_XINPUT_BATTERY_STATE_DISCHARGING),
        level: 55
    )
    let first = controller(firstToken, XInputButtonBits.a, battery: firstBattery)
    let second = controller(secondToken, XInputButtonBits.b, supportsHaptics: true)
    let third = controller(thirdToken, XInputButtonBits.x)
    var detected: [DetectedController] = []
    let api = ScriptedFrameLoop()
    let model = FrameLoopModel(api: api, readControllers: { detected })
    model.startInteractive()

    detected = [first, second]
    api.report(XENORA_LOOP_PAUSED, completed: 1, frame: 0, pixel: 0)
    model.pause()
    var xinputs = api.xinputs
    if xinputs.count == 2 {
        expect(xinputs[0].userIndex == 0 && xinputs[0].input.buttons == XInputButtonBits.a
               && xinputs[0].input.connected == 1 && xinputs[0].input.features == 0,
               "first controller did not occupy XInput slot zero without haptics")
        expect(xinputs[1].userIndex == 1 && xinputs[1].input.buttons == XInputButtonBits.b
               && xinputs[1].input.connected == 1
               && xinputs[1].input.features == UInt8(XENORA_XINPUT_FEATURE_VIBRATION),
               "second controller did not occupy XInput slot one with its haptics capability")
    } else {
        failures.append("two connected controllers produced \(xinputs.count) XInput submissions, expected 2")
    }
    expect(model.controller == first && model.controllers == [first, second],
           "multiplayer publication did not expose slot order with the first controller as primary")
    let batteries = api.xinputBatteries
    expect(batteries.count == 2 && batteries[0].userIndex == 0
           && batteries[0].battery.present == 1
           && batteries[0].battery.state == UInt8(XENORA_XINPUT_BATTERY_STATE_DISCHARGING)
           && batteries[0].battery.level == 55
           && batteries[0].battery.reserved == 0,
           "the first controller battery evidence was not transported atomically with its XInput slot")
    expect(batteries.count == 2 && batteries[1].userIndex == 1 && batteries[1].battery.present == 0
           && batteries[1].battery.state == 0 && batteries[1].battery.level == 0,
           "a controller without battery evidence did not use the canonical absent battery shape")

    detected = [second]
    model.pause()
    xinputs = api.xinputs
    expect(xinputs.count == 3 && xinputs.last?.userIndex == 0
           && xinputs.last?.input.connected == 0 && xinputs.last?.input.buttons == 0,
           "removing the slot-zero controller did not send exactly one neutral disconnect")
    expect(model.controller == second && model.controllers == [second],
           "the remaining slot-one controller did not become the legacy primary controller")

    detected = [second, third]
    model.pause()
    xinputs = api.xinputs
    expect(xinputs.count == 4 && xinputs.last?.userIndex == 0
           && xinputs.last?.input.buttons == XInputButtonBits.x && xinputs.last?.input.connected == 1,
           "a newly connected controller did not reuse the first empty XInput slot")
    expect(model.controller == third && model.controllers == [third, second],
           "slot-order publication did not place the replacement slot-zero controller first")

    model.pause()
    expect(api.xinputs.count == 4, "unchanged multiplayer snapshots were resent")
    return failures
}

@MainActor
private func checkControllerHandoffs() -> [String] {
    var failures: [String] = []
    let left = UInt32(XENORA_INPUT_LEFT)
    let right = UInt32(XENORA_INPUT_RIGHT)
    let up = UInt32(XENORA_INPUT_UP)
    let action = UInt32(XENORA_INPUT_ACTION_B)
    let api = ScriptedFrameLoop()
    func controller(_ buttons: UInt32, x: Float, trigger: Float) -> DetectedController {
        DetectedController(
            identity: ObjectIdentifier(api), name: "Changing controller", supportsHaptics: false,
            snapshot: ControllerSnapshot(buttons: buttons, x: x, y: 0, leftTrigger: trigger, rightTrigger: 0)
        )
    }
    let probe = FramePollingProbe()
    probe.controller = controller(left, x: 0, trigger: 0)
    let model = FrameLoopModel(api: api, readController: { probe.controller })

    func expectInput(_ count: Int, _ buttons: UInt32, connected: UInt8, x: Int16 = 0,
                     trigger: UInt16 = 0, _ context: String) {
        let inputs = api.inputs
        guard inputs.count == count, let input = inputs.last else {
            failures.append("\(context): sent \(inputs.count) inputs, expected \(count)")
            return
        }
        if input.state.buttons != buttons || input.state.connected != connected
            || input.state.stick_x != x || input.left_trigger != trigger {
            failures.append("\(context): wrong effective controller input")
        }
    }

    model.startInteractive()
    model.sendInput(buttons: right)
    expectInput(1, right, connected: 1, "screen takes directional priority")

    probe.controller = controller(right, x: 0, trigger: 0)
    api.report(XENORA_LOOP_PAUSED, completed: 1, frame: 0, pixel: 0)
    model.pause()
    expectInput(1, right, connected: 1, "physical direction masked by the screen stays deduplicated")

    probe.controller = controller(up | action, x: -1, trigger: 0.5)
    api.report(XENORA_LOOP_RUNNING, completed: 1, frame: 0, pixel: 0)
    model.resume()
    expectInput(2, right | up | action, connected: 1, x: -32767, trigger: 32768,
                "physical Up, action and analog input pass through the horizontal overlay")

    model.sendInput(buttons: UInt32(XENORA_INPUT_NONE))
    expectInput(3, up | action, connected: 1, x: -32767, trigger: 32768,
                "Release restores the changed physical direction")
    model.sendInput(buttons: right)

    probe.controller = nil
    api.report(XENORA_LOOP_PAUSED, completed: 1, frame: 0, pixel: 0)
    model.pause()
    expectInput(5, right, connected: 1, "disconnect retains only the screen direction")

    api.fail(.sendInput, Int32(XENORA_INVALID_STATE))
    model.sendInput(buttons: UInt32(XENORA_INPUT_NONE))
    expectInput(6, 0, connected: 0, "Release attempts disconnected neutral state")
    if model.errorMessage != "Failed to send controller input (status 9)." {
        failures.append("failed input update did not report its status")
    }

    api.report(XENORA_LOOP_RUNNING, completed: 1, frame: 0, pixel: 0)
    model.resume()
    expectInput(7, 0, connected: 0, "an unchanged snapshot is retried after a failed send")
    api.report(XENORA_LOOP_PAUSED, completed: 1, frame: 0, pixel: 0)
    model.pause()
    expectInput(7, 0, connected: 0, "a successful retry is deduplicated")

    probe.controller = controller(left, x: 0, trigger: 0)
    model.sendInput(buttons: right)
    expectInput(8, right, connected: 1, "screen direction before session replacement")
    model.start()
    model.pause()
    expectInput(8, right, connected: 1, "standard session does not submit controller input")
    model.startInteractive()
    model.pause()
    expectInput(9, left, connected: 1, "a replacement interactive session starts with only physical input")

    let previousHandle = api.lastInputHandle
    model.startInteractive()
    model.pause()
    expectInput(10, left, connected: 1, "another replacement resends the same held physical input")
    if api.lastInputHandle == previousHandle {
        failures.append("the identical physical input was not submitted to the new session")
    }
    return failures
}

@MainActor
private func checkControllerErrorRecovery() async -> [String] {
    let api = ScriptedFrameLoop()
    let model = FrameLoopModel(api: api, readController: { nil })
    let left = UInt32(XENORA_INPUT_LEFT)
    let release = UInt32(XENORA_INPUT_NONE)
    let inputError = "Failed to send controller input (status 9)."
    let stopError = "Failed to stop frame loop (status 4)."
    let status = Int32(XENORA_INVALID_STATE)
    var inputErrorPublications = 0
    var failures: [String] = []
    let observation = model.$errorMessage.sink { message in
        if message == inputError { inputErrorPublications += 1 }
    }
    defer { observation.cancel() }

    model.startInteractive()
    api.fail(.sendInput, status)
    model.sendInput(buttons: left)
    api.fail(.sendInput, status)
    model.sendInput(buttons: left)
    if model.errorMessage != inputError || api.count(.sendInput) != 2 || inputErrorPublications != 1 {
        failures.append("repeated rejection should retry the send without republishing its diagnostic")
    }
    model.sendInput(buttons: release)
    if model.errorMessage != nil || api.count(.sendInput) != 2 {
        failures.append("returning to the already accepted neutral input should clear the error without a send")
    }

    api.fail(.sendInput, status)
    model.sendInput(buttons: left)
    api.report(XENORA_LOOP_PAUSED, completed: 1, frame: 0, pixel: 0)
    model.pause()
    if model.errorMessage != nil || api.count(.sendInput) != 4 {
        failures.append("an accepted retry should clear the controller input error")
    }
    api.report(XENORA_LOOP_FAILED, completed: 1, frame: 0, pixel: 0)
    model.resume()
    if model.errorMessage != "Fixture frame loop entered the failed state." {
        failures.append("a recovered controller input error should not hide a later terminal failure")
    }

    model.startInteractive()
    api.fail(.sendInput, status)
    model.sendInput(buttons: left)
    api.fail(.stop, Int32(XENORA_INTERNAL_ERROR))
    await model.stop()?.value
    api.fail(.sendInput, status)
    model.sendInput(buttons: left)
    if model.errorMessage != stopError {
        failures.append("a repeated input rejection should not replace a later stop failure")
    }
    model.sendInput(buttons: left)
    if model.errorMessage != stopError || api.count(.sendInput) != 7 {
        failures.append("an accepted retry should preserve a later stop failure")
    }
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
        await model.stop()?.value
        if !(await waitForLoop({ model.state == XENORA_LOOP_STOPPED }))
            || model.journalRecordCount != UInt64(model.completed) {
            failures.append("resumed standard fixture did not stop")
        }
    } else {
        failures.append("standard fixture did not pause")
    }
    if let error = model.errorMessage { failures.append(error) }

    model.start(frameCount: 4)
    if model.journalRecordCount != nil { failures.append("standard restart retained a stale guest journal") }
    if !(await waitForLoop({ model.canStart })) || model.completed != 4
        || model.frameId != 3 || pixels() != expectedPixels(redColumn: 3) || model.errorMessage != nil
        || model.stateDescription != "Finished • 4 verified journal entries" {
        failures.append("standard fixture restart did not finish with frame 3")
    }

    model.startInteractive(frameCount: 10_000)
    if await waitForLoop({ model.isRunning && model.image != nil }) {
        model.pause()
        if model.isPaused {
            let beforeStop = model.completed
            let stop = model.stop()
            if stop == nil || !model.isStopping || model.canStart {
                failures.append("paused interactive fixture did not enter the stopping state")
            }
            await stop?.value
            if model.isStopping || !model.canStart || model.state != XENORA_LOOP_STOPPED
                || model.completed < beforeStop || model.frameId != 0
                || pixels() != expectedPixels(redColumn: 0) || model.errorMessage != nil
                || model.journalRecordCount != UInt64(model.completed) {
                failures.append("interactive stop did not retain its last native frame")
            }
        } else {
            failures.append("interactive fixture did not pause before stop")
        }
    } else {
        failures.append("interactive fixture did not publish a frame before stop")
    }

    model.startInteractive(frameCount: 8)
    if model.journalRecordCount != nil { failures.append("interactive restart retained a stale guest journal") }
    if !(await waitForLoop({ model.canStart })) || model.completed != 8
        || model.frameId != 0 || pixels() != expectedPixels(redColumn: 0) || !model.isInteractive
        || model.errorMessage != nil || model.journalRecordCount != 8 {
        failures.append("interactive fixture did not finish with eight frames at column 0")
    }
    failures += await checkNativeFrameJournal(mode: .standard)
    failures += await checkNativeFrameJournal(mode: .interactive)
    return failures
}

@MainActor
private func checkNativeFrameJournal(mode: FrameLoopMode) async -> [String] {
    let api = NativeFrameLoopAPI()
    var failures: [String] = []
    var handle: xenora_session = 0
    guard api.create(&handle) == XENORA_OK else { return ["could not create a native journal session"] }
    defer { _ = api.destroy(handle) }

    var required: UInt64 = 99
    if api.journalSize(handle, required: &required) != XENORA_INVALID_STATE || required != 0 {
        failures.append("journal query before start did not reject the unstarted loop")
    }
    guard api.start(handle, mode: mode, frames: 10_000) == XENORA_OK,
          api.pause(handle) == XENORA_OK else {
        return failures + ["could not start and pause the native journal fixture"]
    }
    required = 99
    if api.journalSize(handle, required: &required) != XENORA_INVALID_STATE || required != 0 {
        failures.append("journal query while paused did not reject the unfinished loop")
    }
    guard api.resume(handle) == XENORA_OK else {
        return failures + ["could not resume the native journal fixture"]
    }

    var state: UInt32 = 0
    var completed: UInt32 = 0
    var frame: UInt32 = 0
    var rgba = [UInt8](repeating: 0, count: Int(XENORA_FIXTURE_FRAME_WIDTH * XENORA_FIXTURE_FRAME_HEIGHT * 4))
    func readStatus() -> Int32 {
        rgba.withUnsafeMutableBufferPointer {
            api.status(handle, &state, &completed, &frame, $0)
        }
    }
    guard await waitForLoop({
        readStatus() != XENORA_OK || completed >= 4 || state == XENORA_LOOP_FAILED
    }), completed >= 4,
          api.stop(handle) == XENORA_OK, readStatus() == XENORA_OK, state == XENORA_LOOP_STOPPED else {
        return failures + ["native journal fixture did not stop after publishing at least four frames"]
    }
    let count = UInt64(completed)
    if api.journalSize(handle, required: &required) != XENORA_BUFFER_TOO_SMALL || required != count {
        failures.append("native journal sizing did not report the completed record count")
    }
    var sentinel: UInt8 = 0xa5
    if xenora_session_frame_loop_journal(handle, &sentinel, 1, &required) != XENORA_BUFFER_TOO_SMALL
        || required != count || sentinel != 0xa5 {
        failures.append("short native journal buffer was modified or reported the wrong size")
    }
    if xenora_session_frame_loop_journal(handle, nil, 1, &required) != XENORA_INVALID_ARGUMENT {
        failures.append("native journal accepted null output with positive capacity")
    }
    var records = [UInt8](repeating: 0, count: Int(count))
    let status = records.withUnsafeMutableBufferPointer {
        xenora_session_frame_loop_journal(handle, $0.baseAddress, UInt64($0.count), &required)
    }
    let expected: [UInt8]
    switch mode {
    case .standard:
        expected = (0..<Int(count)).map { 0x10 | UInt8($0 % 4) }
    case .interactive:
        expected = [UInt8](repeating: 0x20, count: Int(count))
    }
    if status != XENORA_OK || required != count || records != expected {
        failures.append("native journal did not return the exact \(mode) frame records")
    }
    return failures
}
