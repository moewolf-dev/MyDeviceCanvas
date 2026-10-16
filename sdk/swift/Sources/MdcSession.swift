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

/// N307: surface pixel format exposed by the C ABI. Only `rgb565` is implemented
/// by the host/frame path; the rest decode but are rejected as unsupported.
public enum MdcPixelFormat: Int32 {
    case rgb565 = 0
    case rgb888 = 1
    case rgba8888 = 2
    case gray1 = 3
    case gray4 = 4
    case unknown = -1

    public var isImplemented: Bool { self == .rgb565 }
}

/// N319: one diagnostic record from the link-independent channel.
public struct MdcDiagnostic {
    public enum Kind: Int32 {
        case metric = 0
        case bootInfo = 1
    }
    public let kind: Kind
    public let name: String
    public let value: Double
    public let unit: String
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

    /// N307: the surface pixel format. Only RGB565 is implemented by the host.
    public func surfaceFormat(at index: UInt32) -> MdcPixelFormat {
        guard let raw else { return .unknown }
        return MdcPixelFormat(rawValue: mdc_session_surface_format_at(raw, index)) ?? .unknown
    }

    /// N307: whether the host/frame path implements a format code.
    public func pixelFormatIsImplemented(_ format: MdcPixelFormat) -> Bool {
        mdc_pixel_format_is_implemented(format.rawValue) != 0
    }

    /// N319: number of diagnostics waiting on the link-independent channel.
    public func diagnosticCount() -> UInt32 {
        guard let raw else { return 0 }
        return mdc_session_diagnostic_count(raw)
    }

    /// N319: emit a metric on the simulator device (sim-only; returns the queue
    /// length or a negative code).
    @discardableResult
    public func emitMetric(_ name: String, _ value: Double, unit: String = "") -> Int32 {
        guard let raw else { return -1 }
        return name.withCString { n in
            unit.withCString { u in
                mdc_session_emit_metric(raw, n, value, u)
            }
        }
    }

    /// N319: pop the oldest diagnostic, or nil when the queue is empty.
    public func takeDiagnostic() -> MdcDiagnostic? {
        guard let raw else { return nil }
        var kind: Int32 = 0
        var name = [CChar](repeating: 0, count: 64)
        var unit = [CChar](repeating: 0, count: 64)
        var value = 0.0
        let rc = mdc_session_take_diagnostic(
            raw, &kind, &name, name.count, &value, &unit, unit.count)
        guard rc == 1 else { return nil }
        return MdcDiagnostic(
            kind: MdcDiagnostic.Kind(rawValue: kind) ?? .metric,
            name: String(cString: name),
            value: value,
            unit: String(cString: unit))
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

    /// N63: set a structured vector scene (JSON SceneNode). 0 on success.
    public func setScene(json: String) -> Int32 {
        guard let raw else { return -1 }
        return json.withCString { mdc_set_scene(raw, $0) }
    }

    public func clearScene() -> Int32 {
        guard let raw else { return -1 }
        return mdc_clear_scene(raw)
    }

    /// N246: upload a runtime package (`.mdcpkg` bytes). 0 on send.
    public func installPackage(_ bytes: [UInt8]) -> Int32 {
        guard let raw else { return -1 }
        return bytes.withUnsafeBufferPointer { buffer in
            mdc_install_package(raw, buffer.baseAddress, buffer.count)
        }
    }

    /// N246: 1 when the last accepted package is render-ready, else 0.
    public func packageReady() -> Int32 {
        guard let raw else { return -1 }
        return mdc_package_ready(raw)
    }

    /// N246: read a config value.
    public func getConfig(_ key: String) -> String? {
        guard let raw else { return nil }
        var out = [CChar](repeating: 0, count: 128)
        let n = key.withCString { k in
            out.withUnsafeMutableBufferPointer { buffer in
                mdc_get_config(raw, k, buffer.baseAddress, buffer.count)
            }
        }
        guard n >= 0 else { return nil }
        return String(cString: out)
    }

    /// N246: write a config value. 0 on success.
    public func setConfig(_ key: String, _ value: String) -> Int32 {
        guard let raw else { return -1 }
        return key.withCString { k in
            value.withCString { v in
                mdc_set_config(raw, k, v)
            }
        }
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
