import Foundation

public enum MdcError: Error {
    case createFailed
    case disposed
    case sendFailed(Int32)
}

/// Opaque session. Swift only calls the C ABI and releases the handle in `close` / `deinit`.
public final class MdcSession {
    private var raw: OpaquePointer?

    public init() throws {
        guard let created = mdc_session_create() else {
            throw MdcError.createFailed
        }
        raw = created
    }

    public func close() {
        guard let raw else { return }
        mdc_session_destroy(raw)
        self.raw = nil
    }

    deinit {
        close()
    }

    public func surfaceSize() throws -> (UInt16, UInt16) {
        guard let raw else { throw MdcError.disposed }
        var width: UInt16 = 0
        var height: UInt16 = 0
        let rc = mdc_session_surface_size(raw, &width, &height)
        guard rc == 0 else { throw MdcError.sendFailed(rc) }
        return (width, height)
    }

    public func sendFrame(_ bytes: [UInt8]) throws {
        guard let raw else { throw MdcError.disposed }
        let rc = bytes.withUnsafeBufferPointer { buffer in
            mdc_session_send_frame(raw, buffer.baseAddress, buffer.count)
        }
        guard rc == 0 else { throw MdcError.sendFailed(rc) }
    }

    /// v1 always rejects Scene. A return of -5 is the expected result.
    public func sendScene() -> Int32 {
        guard let raw else { return -1 }
        return mdc_session_send_scene(raw, nil, 0)
    }
}
