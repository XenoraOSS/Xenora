// SPDX-License-Identifier: GPL-3.0-or-later

import CXenora
import GameController
import Testing
@testable import Xenora

@MainActor
struct ControllerInputTests {
    @Test func microDirectionAndPrimaryActionReachTheSnapshot() throws {
        let controller = GCController.withMicroGamepad()
        let pad = try #require(controller.microGamepad)
        pad.dpad.setValueForXAxis(-1, yAxis: 0)
        pad.buttonA.setValue(1)

        let detected = try #require(pollGameController(current: controller, controllers: []))
        #expect(detected.identity == ObjectIdentifier(controller))
        #expect(detected.snapshot.buttons == UInt32(XENORA_INPUT_LEFT) | UInt32(XENORA_INPUT_ACTION_A))
        #expect(detected.snapshot.xinput.buttons == XInputButtonBits.dpadLeft | XInputButtonBits.a)

        pad.dpad.setValueForXAxis(0, yAxis: -1)
        pad.buttonA.setValue(0)
        pad.buttonX.setValue(1)
        let down = try #require(pollGameController(current: controller, controllers: []))
        #expect(down.snapshot.buttons == UInt32(XENORA_INPUT_DOWN))
        #expect(down.snapshot.xinput.buttons == XInputButtonBits.dpadDown | XInputButtonBits.x)

        pad.dpad.setValueForXAxis(0, yAxis: 0)
        pad.buttonX.setValue(0)
        let released = try #require(pollGameController(current: controller, controllers: []))
        #expect(released.snapshot.buttons == 0)
        #expect(released.snapshot.xinput.buttons == 0)
    }


    @Test func batteryEvidenceDistinguishesAbsenceUnknownAndRealValues() {
        #expect(XInputBatteryValues.hostValues(nil) == XInputBatteryValues())
        #expect(XInputBatteryValues().ffi().present == 0)

        let charging = XInputBatteryValues.hostEvidence(state: .charging, level: 0.555)
        #expect(charging.present)
        #expect(charging.state == UInt8(XENORA_XINPUT_BATTERY_STATE_CHARGING))
        #expect(charging.level == 56)
        let wire = charging.ffi()
        #expect(wire.present == 1 && wire.reserved == 0)

        #expect(XInputBatteryValues.hostEvidence(state: .full, level: 1).state ==
                UInt8(XENORA_XINPUT_BATTERY_STATE_FULL))
        #expect(XInputBatteryValues.hostEvidence(state: .discharging, level: 0).state ==
                UInt8(XENORA_XINPUT_BATTERY_STATE_DISCHARGING))
        #expect(XInputBatteryValues.hostEvidence(state: .unknown, level: .nan).level ==
                UInt8(XENORA_XINPUT_BATTERY_LEVEL_UNKNOWN))
        #expect(XInputBatteryValues.hostEvidence(state: .unknown, level: 1.1).level ==
                UInt8(XENORA_XINPUT_BATTERY_LEVEL_UNKNOWN))
    }

    @Test func pollPrefersExtendedControllersThenTheCurrentMicroController() {
        let micro = GCController.withMicroGamepad()
        let otherMicro = GCController.withMicroGamepad()
        let extended = GCController.withExtendedGamepad()
        let otherExtended = GCController.withExtendedGamepad()

        #expect(pollGameController(current: nil, controllers: []) == nil)
        #expect(pollGameController(current: nil, controllers: [micro])?.identity ==
                ObjectIdentifier(micro))
        #expect(pollGameController(current: otherMicro, controllers: [micro, otherMicro])?.identity ==
                ObjectIdentifier(otherMicro))
        #expect(pollGameController(current: micro, controllers: [micro, extended])?.identity ==
                ObjectIdentifier(extended))
        #expect(pollGameController(current: otherExtended, controllers: [extended, otherExtended])?.identity ==
                ObjectIdentifier(otherExtended))

        let multiplayer = pollGameControllers(
            current: micro,
            controllers: [micro, otherMicro, extended, otherExtended]
        )
        #expect(multiplayer.map(\.identity) == [
            ObjectIdentifier(extended),
            ObjectIdentifier(otherExtended),
            ObjectIdentifier(micro),
            ObjectIdentifier(otherMicro),
        ])
        #expect(pollGameControllers(
            current: nil,
            controllers: [micro, otherMicro, extended, otherExtended, GCController.withExtendedGamepad()]
        ).count == 4)
    }

    @Test func extendedControllerStillForwardsButtonsThumbstickAndTriggers() throws {
        let controller = GCController.withExtendedGamepad()
        let pad = try #require(controller.extendedGamepad)
        pad.dpad.setValueForXAxis(1, yAxis: 0)
        pad.buttonB.setValue(1)
        pad.leftThumbstick.setValueForXAxis(-1, yAxis: 1)
        pad.rightThumbstick.setValueForXAxis(0.5, yAxis: -1)
        pad.leftTrigger.setValue(0.25)
        pad.rightTrigger.setValue(0.75)
        pad.buttonMenu.setValue(1)
        pad.leftShoulder.setValue(1)
        pad.buttonY.setValue(1)

        let detected = try #require(pollGameController(current: controller, controllers: []))
        #expect(detected.snapshot.buttons == UInt32(XENORA_INPUT_RIGHT) | UInt32(XENORA_INPUT_ACTION_B))
        #expect(detected.snapshot.x == -32767)
        #expect(detected.snapshot.y == 32767)
        #expect(detected.snapshot.leftTrigger == 0.25)
        #expect(detected.snapshot.rightTrigger == 0.75)
        #expect(detected.snapshot.xinput == XInputSnapshotValues(
            buttons: XInputButtonBits.dpadRight | XInputButtonBits.b | XInputButtonBits.y
                | XInputButtonBits.start | XInputButtonBits.leftShoulder,
            leftTrigger: 64,
            rightTrigger: 191,
            leftX: -32768,
            leftY: 32767,
            rightX: 16384,
            rightY: -32768
        ))
    }
}
