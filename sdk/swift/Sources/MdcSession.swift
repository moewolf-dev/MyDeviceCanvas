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

/// A Form B mirrored frame (RGB565) copied out of the C ABI.
public struct MdcMirrorFrame {
    public let surfaceId: String
    public let frameId: UInt64
    public let width: UInt16
    public let height: UInt16
    public let pixels: [UInt8]
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

    /// Form B: ask the device to start mirroring. Returns the C ABI result.
    public func startMirror(surfaceId: String = "main", maxFps: UInt16 = 15) -> Int32 {
        guard let raw else { return -1 }
        return surfaceId.withCString { mdc_start_mirror(raw, $0, maxFps) }
    }

    public func stopMirror(surfaceId: String = "main") -> Int32 {
        guard let raw else { return -1 }
        return surfaceId.withCString { mdc_stop_mirror(raw, $0) }
    }

    /// Simulator-only: emit one mirrored frame from the FakeDevice.
    public func emitMirrorFrame() -> Int32 {
        guard let raw else { return -1 }
        return mdc_emit_mirror_frame(raw)
    }

    /// Take the newest complete mirror frame, or nil when none is available.
    public func takeMirrorFrame(surfaceId: String = "main") -> MdcMirrorFrame? {
        guard let raw else { return nil }
        guard let (width, height) = try? surfaceSize() else { return nil }
        var outWidth = width
        var outHeight = height
        var frameId: UInt64 = 0
        var pixels = [UInt8](repeating: 0, count: Int(width) * Int(height) * 2)
        let copied = pixels.withUnsafeMutableBufferPointer { buffer in
            mdc_take_mirror_frame(raw, &outWidth, &outHeight, &frameId, buffer.baseAddress, buffer.count)
        }
        guard copied > 0 else { return nil }
        if copied < pixels.count {
            pixels.removeLast(pixels.count - Int(copied))
        }
        return MdcMirrorFrame(
            surfaceId: surfaceId, frameId: frameId,
            width: outWidth, height: outHeight, pixels: pixels)
    }

    public func mirrorStats() -> (received: UInt64, dropped: UInt64) {
        guard let raw else { return (0, 0) }
        var received: UInt64 = 0
        var dropped: UInt64 = 0
        _ = mdc_mirror_stats(raw, &received, &dropped)
        return (received, dropped)
    }

    /// AsyncStream of Form B mirrored frames. A simulator session emits locally.
    public func mirrorFrames(surfaceId: String = "main", maxFps: UInt16 = 15) -> AsyncStream<MdcMirrorFrame> {
        let session = self
        _ = startMirror(surfaceId: surfaceId, maxFps: maxFps)
        return AsyncStream(unfolding: {
            while !Task.isCancelled {
                _ = session.emitMirrorFrame()
                if let frame = session.takeMirrorFrame(surfaceId: surfaceId) {
                    return frame
                }
                try? await Task.sleep(nanoseconds: 5_000_000)
            }
            return nil
        })
    }
}
