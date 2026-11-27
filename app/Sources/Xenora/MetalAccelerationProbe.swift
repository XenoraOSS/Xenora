// SPDX-License-Identifier: GPL-3.0-or-later

import Foundation
import Metal
import simd

enum MetalAccelerationProbeError: Error {
    case noMetalDevice
    case unsupportedRayTracing
    case bufferAllocationFailed
    case accelerationStructureAllocationFailed
    case commandEncodingFailed
    case commandBufferFailed
}

struct MetalAccelerationProbeResult {
    let deviceName: String
    let supportsRaytracing: Bool
    let supportsRaytracingFromRender: Bool
    let accelerationStructureSize: Int
    let buildScratchBufferSize: Int
    let refitScratchBufferSize: Int
}

/// Synchronous host-side probe for the public classic Metal acceleration-
/// structure build path. This is intentionally a construction/lifetime probe,
/// not a ray query, renderer feature or performance benchmark.
enum MetalAccelerationStructureProbe {
    static func buildSingleTriangle() throws -> MetalAccelerationProbeResult {
        guard let device = MTLCreateSystemDefaultDevice() else {
            throw MetalAccelerationProbeError.noMetalDevice
        }
        guard device.supportsRaytracing else {
            throw MetalAccelerationProbeError.unsupportedRayTracing
        }

        var vertices: [SIMD3<Float>] = [
            SIMD3<Float>(0, 0, 0),
            SIMD3<Float>(1, 0, 0),
            SIMD3<Float>(0, 1, 0),
        ]
        let vertexLength = MemoryLayout<SIMD3<Float>>.stride * vertices.count
        guard let vertexBuffer = device.makeBuffer(
            bytes: &vertices,
            length: vertexLength,
            options: .storageModeShared
        ) else {
            throw MetalAccelerationProbeError.bufferAllocationFailed
        }

        let geometry = MTLAccelerationStructureTriangleGeometryDescriptor()
        geometry.vertexBuffer = vertexBuffer
        geometry.vertexBufferOffset = 0
        geometry.vertexStride = MemoryLayout<SIMD3<Float>>.stride
        geometry.triangleCount = 1

        let descriptor = MTLPrimitiveAccelerationStructureDescriptor()
        descriptor.geometryDescriptors = [geometry]
        let sizes = device.accelerationStructureSizes(descriptor: descriptor)
        guard sizes.accelerationStructureSize > 0 else {
            throw MetalAccelerationProbeError.accelerationStructureAllocationFailed
        }
        guard let structure = device.makeAccelerationStructure(
            size: sizes.accelerationStructureSize
        ) else {
            throw MetalAccelerationProbeError.accelerationStructureAllocationFailed
        }
        guard let scratchBuffer = device.makeBuffer(
            length: sizes.buildScratchBufferSize,
            options: .storageModePrivate
        ) else {
            throw MetalAccelerationProbeError.bufferAllocationFailed
        }
        guard let queue = device.makeCommandQueue(),
              let commandBuffer = queue.makeCommandBuffer(),
              let encoder = commandBuffer.makeAccelerationStructureCommandEncoder()
        else {
            throw MetalAccelerationProbeError.commandEncodingFailed
        }

        encoder.build(
            accelerationStructure: structure,
            descriptor: descriptor,
            scratchBuffer: scratchBuffer,
            scratchBufferOffset: 0
        )
        encoder.endEncoding()
        commandBuffer.commit()
        withExtendedLifetime((vertexBuffer, scratchBuffer, descriptor, structure)) {
            commandBuffer.waitUntilCompleted()
        }
        guard commandBuffer.status == .completed else {
            if let error = commandBuffer.error {
                throw error
            }
            throw MetalAccelerationProbeError.commandBufferFailed
        }

        return MetalAccelerationProbeResult(
            deviceName: device.name,
            supportsRaytracing: device.supportsRaytracing,
            supportsRaytracingFromRender: device.supportsRaytracingFromRender,
            accelerationStructureSize: sizes.accelerationStructureSize,
            buildScratchBufferSize: sizes.buildScratchBufferSize,
            refitScratchBufferSize: sizes.refitScratchBufferSize
        )
    }
}
