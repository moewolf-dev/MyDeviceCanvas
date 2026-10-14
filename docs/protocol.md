# MyDeviceCanvas Protocol v1

The wire format is a byte stream. Every packet starts with the 20-byte little-endian header:

| offset | size | field |
|---:|---:|---|
| 0 | 2 | magic `MD` |
| 2 | 1 | major |
| 3 | 1 | minor |
| 4 | 1 | message type |
| 5 | 1 | reserved, zero |
| 6 | 2 | flags |
| 8 | 4 | request ID |
| 12 | 4 | payload length |
| 16 | 4 | FNV-1a-style integrity checksum |

Integers are little-endian. Control payloads are CBOR and are limited to 4096 bytes. Frame and tile payloads are raw RGB565 little-endian bytes, never base64. The default chunk limit is 16 KiB. A receiver must reject invalid magic, unsupported major versions, unknown message types, lengths above its negotiated limit, truncation and checksum failures without allocating the advertised payload first.

Supported types are HELLO, CAPABILITIES, FRAME, TILE, MIRRORFRAME, INPUT, OTA, MIRRORCTRL, PING, PONG, ACK and ERROR. Input and OTA are reserved in 0.1 and return `Unsupported` unless capabilities explicitly enable them.

## Form B: mirror upstream (N03)

`MIRRORFRAME` (0x12) is a **device → host** image message. It reuses the `FRAME`
payload phases (0 legacy, 1 begin, 2 chunk, 3 commit); the message type keeps a
mirrored frame distinct from a host-pushed frame. `MIRRORCTRL` (0x31) is a
**host → device** CBOR `MirrorCommand` (`action: start|stop`) that a device
rejects with `E_UNSUPPORTED` when it did not advertise mirror.

`CAPABILITIES` gains `mirror { mode, max_fps, max_width, max_height,
pixel_formats, recommended_transport }`, `local_display_fps`, `mirror_fps` and
`mirror_bandwidth`. They are optional on the wire: a device that never sent them
decodes as mirror-disabled (empty `mode`). `local_display_fps` and `mirror_fps`
are independent; `mirror_bandwidth` is advisory. See
[render-topology-decision.md](render-topology-decision.md) for the full mode
contract.

## Renderer capability (N17)

`CAPABILITIES` optionally carries `renderer { sprite_2d, mesh2d, mesh3d,
skeleton, max_texture, max_vertices }`: the local rendering a device supports.
It is `Option` and `skip_serializing_if = "Option::is_none"`, so a pre-renderer
device produces a 15-entry map and a device that advertises renderer produces a
16-entry map. The host model compiler reads it to pick a target backend
(`mdc-model::RendererCapability::from_wire`); an absent capability is treated as
sprite-only. Rust and C++ share the exact encoding
(`cbor_control_test.cpp` reproduces the `mdc-protocol` bytes).
