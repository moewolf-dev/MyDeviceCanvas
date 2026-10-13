import Foundation

public enum MdcError: Error {
    case createFailed
    case disposed
    case sendFailed(Int32)
}

public struct MdcUiEvent {
    public let kind: Int32
    public let phase: String
    public let x: UInt16
    public let y: UInt16
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

    /// TCP simulator peer (`host:port`). The peer must already be listening.
    public init(tcp address: String) throws {
        let created = address.withCString { text in
            mdc_session_connect_tcp(text)
        }
        guard let created else {
            throw MdcError.createFailed
        }
        raw = created
    }

    /// Named simulator board such as `dual-sim`. The default `init()` stays 480×320.
    public init(boardId: String) throws {
        let created = boardId.withCString { text in
            mdc_session_create_board(text)
        }
        guard let created else {
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

    public func surfaceCount() -> UInt32 {
        guard let raw else { return 0 }
        return mdc_session_surface_count(raw)
    }

    public func surfaceAt(_ index: UInt32) throws -> (String, UInt16, UInt16) {
        guard let raw else { throw MdcError.disposed }
        var id = [CChar](repeating: 0, count: 32)
        var width: UInt16 = 0
        var height: UInt16 = 0
        let rc = id.withUnsafeMutableBufferPointer { buffer in
            mdc_session_surface_at(raw, index, buffer.baseAddress, buffer.count, &width, &height)
        }
        guard rc == 0 else { throw MdcError.sendFailed(rc) }
        return (String(cString: id), width, height)
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

    public func simulateInput(phase: String, x: UInt16, y: UInt16) -> Int32 {
        guard let raw else { return -1 }
        return phase.withCString { text in
            mdc_session_simulate_input(raw, text, x, y)
        }
    }

    public func pollEvent() -> MdcUiEvent? {
        guard let raw else { return nil }
        var kind: Int32 = 0
        var deviceId = [CChar](repeating: 0, count: 64)
        var phase = [CChar](repeating: 0, count: 16)
        var x: UInt16 = 0
        var y: UInt16 = 0
        let rc = deviceId.withUnsafeMutableBufferPointer { deviceBuf in
            phase.withUnsafeMutableBufferPointer { phaseBuf in
                mdc_session_poll_event(
                    raw, &kind, deviceBuf.baseAddress, deviceBuf.count,
                    phaseBuf.baseAddress, phaseBuf.count, &x, &y)
            }
        }
        guard rc == 1 else { return nil }
        return MdcUiEvent(kind: kind, phase: String(cString: phase), x: x, y: y)
    }

    /// Poll the C event queue until the task is cancelled.
    public func events() -> AsyncStream<MdcUiEvent> {
        let session = self
        return AsyncStream(unfolding: {
            while !Task.isCancelled {
                if let event = session.pollEvent() {
                    return event
                }
                try? await Task.sleep(nanoseconds: 5_000_000)
            }
            return nil
        })
    }
}
