// SPDX-License-Identifier: GPL-3.0-or-later

import Metal
import Testing
@testable import Xenora

struct MetalAccelerationProbeTests {
    @Test func singleTrianglePrimitiveBuildCompletesTwiceOrUnsupportedIsReported() throws {
        do {
            let first = try MetalAccelerationStructureProbe.buildSingleTriangle()
            let second = try MetalAccelerationStructureProbe.buildSingleTriangle()

            #expect(!first.deviceName.isEmpty)
            #expect(first.supportsRaytracing)
            #expect(first.accelerationStructureSize > 0)
            #expect(first.buildScratchBufferSize > 0)
            #expect(first.accelerationStructureSize == second.accelerationStructureSize)
            #expect(first.buildScratchBufferSize == second.buildScratchBufferSize)
            #expect(first.refitScratchBufferSize == second.refitScratchBufferSize)
        } catch MetalAccelerationProbeError.unsupportedRayTracing {
            guard let device = MTLCreateSystemDefaultDevice() else {
                throw MetalAccelerationProbeError.noMetalDevice
            }
            #expect(!device.supportsRaytracing)
        }
    }
}
