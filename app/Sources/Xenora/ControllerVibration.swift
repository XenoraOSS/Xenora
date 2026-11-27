// SPDX-License-Identifier: GPL-3.0-or-later

import CXenora
import CoreHaptics
import GameController

struct XInputVibrationCommand: Equatable {
    let sequence: UInt32
    let leftMotor: UInt16
    let rightMotor: UInt16
    let connected: Bool
    let enabled: Bool

    init(
        sequence: UInt32 = 0,
        leftMotor: UInt16 = 0,
        rightMotor: UInt16 = 0,
        connected: Bool = false,
        enabled: Bool = true
    ) {
        self.sequence = sequence
        self.leftMotor = leftMotor
        self.rightMotor = rightMotor
        self.connected = connected
        self.enabled = enabled
    }

    init(_ ffi: xenora_xinput_vibration) {
        self.init(
            sequence: ffi.sequence,
            leftMotor: ffi.left_motor,
            rightMotor: ffi.right_motor,
            connected: ffi.connected != 0,
            enabled: ffi.enabled != 0
        )
    }

    var leftIntensity: Float { enabled ? Float(leftMotor) / 65535 : 0 }
    var rightIntensity: Float { enabled ? Float(rightMotor) / 65535 : 0 }
}

@MainActor
protocol ControllerVibrationSink: AnyObject {
    var lastError: String? { get }
    func apply(command: XInputVibrationCommand, haptics: GCDeviceHaptics?)
    func reset()
}

private struct VibrationOutputError: Error, CustomStringConvertible {
    let description: String
}

/// Best-effort continuous rumble output through GameController's Core Haptics
/// engines. XInput motors do not map exactly to Apple's intensity/sharpness
/// model; this deliberately maps motor speed to intensity only. Left and right
/// handle localities are used when present, otherwise one default actuator
/// receives the stronger motor intensity. No hardware effect is implied when
/// engine creation fails.
@MainActor
private final class ContinuousMotor {
    private let engine: CHHapticEngine
    private let player: CHHapticPatternPlayer
    private var running = false
    private var intensity: Float = 0

    init(haptics: GCDeviceHaptics, locality: GCHapticsLocality, sharpness: Float) throws {
        guard let engine = haptics.createEngine(withLocality: locality) else {
            throw VibrationOutputError(description: "GameController did not create a haptic engine")
        }
        self.engine = engine
        engine.playsHapticsOnly = true
        do {
            try engine.start()
            let event = CHHapticEvent(
                eventType: .hapticContinuous,
                parameters: [
                    CHHapticEventParameter(parameterID: .hapticIntensity, value: 1),
                    CHHapticEventParameter(parameterID: .hapticSharpness, value: sharpness),
                    CHHapticEventParameter(parameterID: .sustained, value: 1),
                ],
                relativeTime: 0,
                duration: 0
            )
            let pattern = try CHHapticPattern(events: [event], parameters: [])
            player = try engine.makePlayer(with: pattern)
        } catch {
            engine.stop(completionHandler: nil)
            throw error
        }
    }

    func setIntensity(_ next: Float) throws {
        let clamped = min(1, max(0, next))
        guard clamped > 0 else {
            if running {
                try player.stop(atTime: CHHapticTimeImmediate)
                running = false
            }
            intensity = 0
            return
        }
        if !running {
            try player.start(atTime: CHHapticTimeImmediate)
            running = true
        }
        guard intensity != clamped else { return }
        try player.sendParameters(
            [CHHapticDynamicParameter(parameterID: .hapticIntensityControl, value: clamped, relativeTime: 0)],
            atTime: CHHapticTimeImmediate
        )
        intensity = clamped
    }

    func stop() {
        try? player.stop(atTime: CHHapticTimeImmediate)
        engine.stop(completionHandler: nil)
    }
}

@MainActor
final class GameControllerVibrationSink: ControllerVibrationSink {
    private enum Layout {
        case separate(left: ContinuousMotor, right: ContinuousMotor)
        case combined(ContinuousMotor)
    }

    private var haptics: GCDeviceHaptics?
    private var layout: Layout?
    private(set) var lastError: String?

    func apply(command: XInputVibrationCommand, haptics nextHaptics: GCDeviceHaptics?) {
        guard command.connected, let nextHaptics else {
            reset()
            return
        }
        if haptics !== nextHaptics {
            reset()
            haptics = nextHaptics
        }
        do {
            if layout == nil {
                layout = try makeLayout(haptics: nextHaptics)
            }
            switch layout {
            case let .separate(left, right)?:
                try left.setIntensity(command.leftIntensity)
                try right.setIntensity(command.rightIntensity)
            case let .combined(motor)?:
                try motor.setIntensity(max(command.leftIntensity, command.rightIntensity))
            case nil:
                throw VibrationOutputError(description: "haptic layout was not initialized")
            }
            lastError = nil
        } catch {
            let message = "Controller haptics unavailable: \(error)"
            stopLayout()
            haptics = nil
            lastError = message
        }
    }

    func reset() {
        stopLayout()
        haptics = nil
        lastError = nil
    }

    private func stopLayout() {
        if let layout {
            switch layout {
            case let .separate(left, right):
                left.stop()
                right.stop()
            case let .combined(motor):
                motor.stop()
            }
        }
        layout = nil
    }

    private func makeLayout(haptics: GCDeviceHaptics) throws -> Layout {
        let localities = haptics.supportedLocalities
        if localities.contains(GCHapticsLocality.leftHandle),
           localities.contains(GCHapticsLocality.rightHandle)
        {
            var left: ContinuousMotor?
            do {
                left = try ContinuousMotor(haptics: haptics, locality: GCHapticsLocality.leftHandle, sharpness: 0.25)
                let right = try ContinuousMotor(haptics: haptics, locality: GCHapticsLocality.rightHandle, sharpness: 0.75)
                guard let left else {
                    throw VibrationOutputError(description: "left haptic motor was not initialized")
                }
                return .separate(left: left, right: right)
            } catch {
                left?.stop()
                throw error
            }
        }
        return .combined(
            try ContinuousMotor(haptics: haptics, locality: GCHapticsLocality.default, sharpness: 0.5)
        )
    }
}
