# Swift SDK notes

There is no separate Swift Package yet. Host apps should link the C ABI from `sdk/c`:

| Artifact | Path |
|---|---|
| Header | `sdk/c/include/mdc.h` |
| Library | `libmdc_c` (cdylib / staticlib via `cargo build -p mdc_c`) |

## Bridging

1. Build: `cargo build -p mdc_c --release`
2. Add `mdc.h` to the Xcode / SPM clang module map
3. Wrap opaque `MdcSession*` in a Swift `final class` with `deinit { mdc_session_destroy(...) }`

```swift
// Illustrative — not compiled in this repo
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

License: Apache-2.0
