# Swift SDK notes

Host apps link the C ABI from `sdk/c`. `Sources/MdcSession.swift` is the wrapper compiled by `tools/check.sh` when `swiftc` is available:

| Artifact | Path |
|---|---|
| Header | `sdk/c/include/mdc.h` |
| Library | `libmdc_c` (cdylib / staticlib via `cargo build -p mdc_c`) |

## Bridging

1. Build: `cargo build -p mdc_c --release`
2. Add `mdc.h` to the Xcode / SPM clang module map
3. Wrap opaque `MdcSession*` in a Swift `final class` with `deinit { mdc_session_destroy(...) }`

```swift
final class MdcSession {
  private let raw: OpaquePointer
  init() throws {
    guard let p = mdc_session_create() else { throw MdcError.createFailed }
    raw = OpaquePointer(p)
  }
  deinit { mdc_session_destroy(UnsafeMutablePointer(raw)) }
  func sendFrame(_ bytes: Data) throws {
    let rc = bytes.withUnsafeBytes { buf in
      mdc_session_send_frame(UnsafeMutablePointer(raw), buf.bindMemory(to: UInt8.self).baseAddress, bytes.count)
    }
    guard rc == 0 else { throw MdcError.sendFailed(rc) }
  }
  func sendTile(baseFrameId: UInt64, x: UInt16, y: UInt16, width: UInt16, height: UInt16, bytes: Data) throws {
    let rc = bytes.withUnsafeBytes { buf in
      mdc_session_send_tile(UnsafeMutablePointer(raw), baseFrameId, x, y, width, height,
                            buf.bindMemory(to: UInt8.self).baseAddress, bytes.count)
    }
    guard rc == 0 else { throw MdcError.sendFailed(rc) }
  }
}
```

Default session is **simulator FakeDevice** (same as CLI `--sim`). Physical transports are not exposed through the C ABI in 0.1.
Also: `mdc_session_current_frame_id`, `mdc_session_surface_size`, `mdc_session_needs_full_frame`.

N366: `MdcSession(webSocket: "ws://host:port")` connects over WebSocket (C ABI
`mdc_session_connect_ws`), alongside `MdcSession(tcp:)`. N409:
`session.sendInput(pointerId:phase:x:y:)` sends a pointer event to a touch-capable
device (C ABI `mdc_session_send_input`).

## Pixel format (N307)

`MdcSession.swift` exposes the surface pixel format enum:

```swift
let format = session.surfaceFormat(at: 0)          // .rgb565 on the default board
let ok = session.pixelFormatIsImplemented(format)  // true only for .rgb565
```

`MdcPixelFormat` is `rgb565` (implemented) plus `rgb888` / `rgba8888` / `gray1` /
`gray4` / `unknown` (decode but are rejected by the host/frame path). The C ABI
function is `mdc_session_surface_format_at`; the enum mirrors `MdcPixelFormat` in
`sdk/c/include/mdc.h`.

## Diagnostics (N319)

`MdcSession.swift` exposes the link-independent diagnostics channel:

```swift
session.emitMetric("fps", 29.5, unit: "fps")   // sim-only; returns queue length
if let d = session.takeDiagnostic() {          // nil when empty
  // d.kind == .metric | .bootInfo, d.name, d.value, d.unit
}
session.diagnosticCount()
```

The C ABI is `mdc_session_emit_metric` / `mdc_session_take_diagnostic` /
`mdc_session_diagnostic_count`.

## Package + config (N246)

`MdcSession.swift` also wraps the newer C ABI calls:

```swift
try session.setConfig("brightness", "200")      // returns 0 / negative error
let value = session.getConfig("brightness")     // "200" or nil
let rc = session.installPackage(packageBytes)   // 0 on send; check readiness:
if session.packageReady() == 1 { /* renderer ready */ }
```

`installPackage` acceptance is asynchronous (the device replies after commit), so
poll `packageReady()` before treating the renderer as ready. `getConfig` returns
`nil` for an unknown key; `setConfig` returns a negative code when the device
rejects the value.

License: Apache-2.0
