// SPDX-License-Identifier: GPL-3.0-or-later
import Darwin
import Foundation

enum InspectionInputError: Error, Equatable {
    case notRegular
    case tooLarge
    case cannotRead(Int32)
}

enum InspectionInput {
    static func read(_ url: URL, limit: Int) throws -> Data {
        precondition(limit >= 0)
        return try url.withUnsafeFileSystemRepresentation { path in
            guard url.isFileURL, let path else { throw InspectionInputError.notRegular }
            var info = stat()
            guard Darwin.fstatat(AT_FDCWD, path, &info, 0) == 0 else {
                throw InspectionInputError.cannotRead(errno)
            }
            guard info.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG) else {
                throw InspectionInputError.notRegular
            }

            // Follow regular-file symlinks. Nonblocking open prevents a
            // FIFO substituted after the path check from stalling admission.
            let descriptor = Darwin.open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC)
            guard descriptor >= 0 else { throw InspectionInputError.cannotRead(errno) }
            defer { _ = Darwin.close(descriptor) }
            guard Darwin.fstat(descriptor, &info) == 0 else {
                throw InspectionInputError.cannotRead(errno)
            }
            guard info.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG) else {
                throw InspectionInputError.notRegular
            }
            guard info.st_size <= off_t(limit) else { throw InspectionInputError.tooLarge }

            return try readContents(from: descriptor, limit: limit)
        }
    }

    /// The caller owns the validated descriptor. Enforce the bound even if the
    /// file's reported size no longer matches the bytes available to read.
    static func readContents(from descriptor: Int32, limit: Int) throws -> Data {
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let remaining = limit - data.count
            let request = remaining == 0 ? 1 : min(remaining, buffer.count)
            let count = buffer.withUnsafeMutableBytes {
                Darwin.read(descriptor, $0.baseAddress, request)
            }
            if count == 0 { return data }
            if count < 0 {
                let code = errno
                if code == EINTR { continue }
                throw InspectionInputError.cannotRead(code)
            }
            guard count <= remaining else { throw InspectionInputError.tooLarge }
            data.append(contentsOf: buffer.prefix(count))
        }
    }
}
