// SPDX-License-Identifier: GPL-3.0-or-later
import Foundation
import Testing
@testable import Xenora

@MainActor
struct ProgramRunModelTests {
    private struct Invocation: Equatable, Sendable {
        let url: URL
        let trace: Bool
        let arguments: [String]
        let standardInput: URL?
    }

    private actor InvocationLog {
        private(set) var entries: [Invocation] = []

        func append(_ invocation: Invocation) {
            entries.append(invocation)
        }
    }

    @Test func keepsTheWorkerOccupiedWhenTheSelectionChanges() async throws {
        let firstURL = URL(fileURLWithPath: "/nonexistent/first.exe")
        let blockedURL = URL(fileURLWithPath: "/nonexistent/blocked.exe")
        let nextURL = URL(fileURLWithPath: "/nonexistent/next.exe")
        let firstInput = URL(fileURLWithPath: "/nonexistent/first input.dat")
        let editedInput = URL(fileURLWithPath: "/nonexistent/edited input.dat")
        let firstOutcome = outcome(output: "previous selection", status: 23)
        let nextOutcome = outcome(output: "current selection", status: 0)
        let firstStarted = AsyncStream<Void>.makeStream()
        let firstGate = AsyncStream<Void>.makeStream()
        let nextGate = AsyncStream<Void>.makeStream()
        defer {
            firstStarted.continuation.finish()
            firstGate.continuation.finish()
            nextGate.continuation.finish()
        }
        let invocations = InvocationLog()
        let model = ProgramRunModel(worker: { url, trace, arguments, standardInput in
            await invocations.append(Invocation(
                url: url, trace: trace, arguments: arguments, standardInput: standardInput
            ))
            switch url {
            case firstURL:
                firstStarted.continuation.yield(())
                var gate = firstGate.stream.makeAsyncIterator()
                _ = await gate.next()
                return firstOutcome
            case nextURL:
                var gate = nextGate.stream.makeAsyncIterator()
                _ = await gate.next()
                return nextOutcome
            default:
                return firstOutcome
            }
        })

        _ = model.addArgument()
        model.arguments[0].value = "  first argument  "
        _ = model.addArgument()
        #expect(model.standardInput == nil)
        model.standardInput = firstInput
        let first = try #require(model.run(firstURL, trace: true))
        model.arguments[0].value = "edited argument"
        model.standardInput = editedInput
        var started = firstStarted.stream.makeAsyncIterator()
        _ = await started.next()
        #expect(model.running)
        #expect(model.fileName == "first.exe")
        let blockedBeforeClear = model.run(blockedURL, trace: false)
        #expect(blockedBeforeClear == nil)
        #expect(model.fileName == "first.exe")

        model.clear()
        #expect(model.running)
        #expect(model.fileName == nil)
        #expect(model.result == nil)
        #expect(model.standardInput == nil)
        let blockedAfterClear = model.run(blockedURL, trace: false)
        #expect(blockedAfterClear == nil)
        #expect(model.fileName == nil)

        firstGate.continuation.finish()
        await first.value
        _ = await blockedBeforeClear?.value
        _ = await blockedAfterClear?.value
        #expect(!model.running)
        #expect(model.fileName == nil)
        #expect(model.result == nil)
        let firstInvocation = Invocation(
            url: firstURL, trace: true, arguments: ["  first argument  ", ""], standardInput: firstInput
        )
        #expect(await invocations.entries == [firstInvocation])

        let next = try #require(model.run(nextURL, trace: false))
        #expect(model.running)
        #expect(model.fileName == "next.exe")
        #expect(model.result == nil)
        nextGate.continuation.finish()
        await next.value
        #expect(!model.running)
        #expect(model.fileName == "next.exe")
        #expect(model.result == nextOutcome)
        let nextInvocation = Invocation(
            url: nextURL, trace: false, arguments: ["edited argument", ""], standardInput: nil
        )
        #expect(await invocations.entries == [firstInvocation, nextInvocation])
    }

    @Test func aLaunchFailureLeavesTheSlotAvailableForRetry() async throws {
        let url = URL(fileURLWithPath: "/nonexistent/retry.exe")
        let failure = ProgramRunResult(
            exitStatus: nil, standardOutput: "", standardError: "", truncated: false,
            lastStage: nil, launchError: "runner not found", hostMilliseconds: 1
        )
        let model = ProgramRunModel(worker: { _, _, _, _ in failure })
        for _ in 0..<2 {
            let run = try #require(model.run(url, trace: false))
            await run.value
            #expect(model.result == failure)
            #expect(!model.running)
        }
    }

    private func outcome(output: String, status: Int32) -> ProgramRunResult {
        ProgramRunResult(
            exitStatus: status,
            standardOutput: output,
            standardError: "",
            truncated: false,
            lastStage: nil,
            launchError: nil,
            hostMilliseconds: 1
        )
    }
}
