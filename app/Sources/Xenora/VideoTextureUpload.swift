// SPDX-License-Identifier: GPL-3.0-or-later

import CoreVideo
import Foundation
import Metal

enum VideoTextureUploadError: Error, Equatable {
    case noMetalDevice
    case unsupportedPixelFormat
    case emptyPixelBuffer
    case missingPixelBuffer
    case textureCreationFailed
}

/// Bounded copy-based upload of decoded BGRA video frames into native Metal
/// textures. This is deliberately not a presentation pipeline: it performs no
/// scheduling, shader sampling, color conversion or synchronization policy.
@MainActor
final class VideoTextureUploader {
    private let device: MTLDevice

    init() throws {
        guard let device = MTLCreateSystemDefaultDevice() else {
            throw VideoTextureUploadError.noMetalDevice
        }
        self.device = device
    }

    func upload(frame: VideoFrame) throws -> MTLTexture {
        guard let pixelBuffer = frame.pixelBuffer else {
            throw VideoTextureUploadError.missingPixelBuffer
        }
        return try upload(pixelBuffer: pixelBuffer)
    }

    func upload(pixelBuffer: CVPixelBuffer) throws -> MTLTexture {
        guard CVPixelBufferGetPixelFormatType(pixelBuffer) == kCVPixelFormatType_32BGRA else {
            throw VideoTextureUploadError.unsupportedPixelFormat
        }
        let width = CVPixelBufferGetWidth(pixelBuffer)
        let height = CVPixelBufferGetHeight(pixelBuffer)
        guard width > 0 && height > 0 else {
            throw VideoTextureUploadError.emptyPixelBuffer
        }

        let descriptor = MTLTextureDescriptor.texture2DDescriptor(
            pixelFormat: .bgra8Unorm,
            width: width,
            height: height,
            mipmapped: false
        )
        descriptor.usage = [.shaderRead]
        descriptor.storageMode = .shared
        guard let texture = device.makeTexture(descriptor: descriptor) else {
            throw VideoTextureUploadError.textureCreationFailed
        }

        CVPixelBufferLockBaseAddress(pixelBuffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(pixelBuffer) else {
            throw VideoTextureUploadError.emptyPixelBuffer
        }
        let region = MTLRegion(
            origin: MTLOrigin(x: 0, y: 0, z: 0),
            size: MTLSize(width: width, height: height, depth: 1)
        )
        texture.replace(
            region: region,
            mipmapLevel: 0,
            withBytes: base,
            bytesPerRow: CVPixelBufferGetBytesPerRow(pixelBuffer)
        )
        return texture
    }
}
