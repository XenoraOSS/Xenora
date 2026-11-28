// SPDX-License-Identifier: GPL-3.0-or-later

import CryptoKit
import Foundation
import Testing
@testable import Xenora

@MainActor
struct AuthoredPESessionBundleTests {
    @Test(.enabled(if: ProcessInfo.processInfo.environment["XENORA_PE_SESSION_TEST_DIRECTORY"] != nil))
    func bundledReferenceRunnerReplaysAnAuthoredPackageThroughTheAppModel() async throws {
        try await replay(backend: .reference)
    }

    @Test(.enabled(if: ProcessInfo.processInfo.environment["XENORA_PE_SESSION_TEST_METAL"] == "1"))
    func bundledMetalRunnerReplaysAnAuthoredPackageThroughTheAppModel() async throws {
        try await replay(backend: .metal)
    }

    private func replay(backend: AuthoredPESessionBackend) async throws {
        let environment = ProcessInfo.processInfo.environment
        let directory = URL(fileURLWithPath: try #require(environment["XENORA_PE_SESSION_TEST_DIRECTORY"]))
        let executable = directory.appendingPathComponent("authored-session-package.exe")
        let companion = directory.appendingPathComponent("xenora-authored-companion.dll")
        let runner = URL(fileURLWithPath: try #require(environment["XENORA_PE_SESSION"]))
        let model = AuthoredPESessionModel(executable: runner)
        defer { model.cancel() }

        let root = try pinned(executable)
        let dll = try pinned(companion)
        model.start(AuthoredPESessionConfiguration(
            executable: root,
            companions: [AuthoredPESessionImage(url: companion, sha256: String(repeating: "0", count: 64))],
            backend: backend
        ))
        try await reach(.failed, with: model)
        #expect(model.statusText.contains("SHA-256"))
        #expect(model.frame == nil)

        model.start(AuthoredPESessionConfiguration(executable: root, companions: [dll], backend: backend))
        try await reach(.ready, with: model)
        for (position, input, blue) in [(1, UInt32(10), UInt8(0x4a)), (2, 21, 0x5f), (3, 10, 0x69)] {
            model.send(input)
            try await reach(.ready, with: model)
            let frame = try #require(model.frame)
            let expected = expectedPixels(index: position, input: input, blue: blue)
            #expect(frame.index == position && frame.width == 4 && frame.height == 3)
            #expect(frame.rgba == expected)
            #expect(frame.sha256 == digest(expected))
            #expect(frame.image?.width == 4 && frame.image?.height == 3)
        }
        model.finish()
        try await reach(.finished, with: model)
        #expect(model.frames == 3)
    }

    private func reach(_ phase: AuthoredPESessionModel.Phase, with model: AuthoredPESessionModel) async throws {
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: .seconds(20))
        while model.phase != phase && model.phase != .failed && clock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(model.phase == phase, "Session state is \(model.phase). \(model.statusText)")
    }

    private func expectedPixels(index: Int, input: UInt32, blue: UInt8) -> Data {
        let triangleColumn = input.isMultiple(of: 2) ? 0 : 3
        var rgba = Data()
        for _ in 0..<3 {
            for column in 0..<4 {
                let pixel: [UInt8] = column == triangleColumn
                    ? [255, 0, 0, 255]
                    : [UInt8(input), UInt8(index), blue, 255]
                rgba.append(contentsOf: pixel)
            }
        }
        return rgba
    }

    private func pinned(_ url: URL) throws -> AuthoredPESessionImage {
        AuthoredPESessionImage(url: url, sha256: digest(try Data(contentsOf: url)))
    }

    private func digest(_ bytes: Data) -> String {
        SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined()
    }
}
