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

Supported types are HELLO, CAPABILITIES, FRAME, TILE, MIRRORFRAME, INPUT, OTA, MIRRORCTRL, PACKAGE, CONFIG, SCENE, PING, PONG, ACK and ERROR. Input and OTA are reserved in 0.1 and return `Unsupported` unless capabilities explicitly enable them. `TELEMETRY`, `METRIC`, `CRASH` and `BOOTINFO` (`0x60..0x63`, N303) are **reserved**: the decoder accepts the values so the enum is stable, but no payload is implemented yet (see below).

## Reserved diagnostics (N303) and diagnostics channel (N316)

`TELEMETRY`/`METRIC`/`CRASH`/`BOOTINFO` are device → host types. `METRIC`
(`MetricReport`), `BOOTINFO` (`BootInfo`) and `TELEMETRY` (`TelemetryEvent`, N335)
now carry CBOR payloads; `CRASH` remains **reserved** with no payload. Diagnostics
travel on their own message types so they never mix with a frame transaction, and a
receiver keeps them in a bounded queue (`Session::take_diagnostics`, 64 records).
A `MetricReport`/`BootInfo`/`TelemetryEvent` `boot_id`/`run_id` also feeds the
reboot detection of N304. Diagnostics should still use a channel **independent of
the link under test**; see
[transport-scope.md](transport-scope.md#reserved-diagnostics--telemetry-n303--not-implemented).

## Reboot semantics (N304)

`HELLO` carries a host `boot_id`; `CAPABILITIES` carries the device `boot_id` and
a `run_id`; `ACK` carries `run_id` and a monotonic `event_seq`. All three are
additive and omitted on the wire when 0, so a pre-N304 peer is byte-identical. A
`Session` records the device `boot_id`; a change means the device rebooted, so the
previous run's pending ACKs and held frames are discarded and an ACK stamped with
a different `run_id` is ignored.

## Pixel format (N307)

A `SURFACE.pixel_format` is a `PixelFormat` enum serialized as its canonical wire
string. Only `RGB565` is implemented by the host/frame path; `RGB888`, `RGBA8888`,
`GRAY1`, `GRAY4` and any other name decode but are rejected with an explicit
unsupported error. SDKs expose the enum (CLI/Node `implemented`, C
`mdc_session_surface_format_at`, Swift `MdcPixelFormat`).

## Display profile (N308)

A `SURFACE` may advertise `preferred_tile_size` (0 = no preference),
`supports_partial_update` and `supports_full_framebuffer`. All are additive and
omitted when default. The host adapts its tile cut to the advertised
`preferred_tile_size` (falling back to its own default) instead of assuming the
optimal 16/32 size, which is a per-board measurement.

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
skeleton, dqs?, max_texture, max_vertices }`: the local rendering a device
supports. `dqs` (N726) is additive and omitted when false, so a device that does
not advertise it produces the same bytes as before.
It is `Option` and `skip_serializing_if = "Option::is_none"`, so a pre-renderer
device produces a 15-entry map and a device that advertises renderer produces a
16-entry map. The host model compiler reads it to pick a target backend
(`mdc-model::RendererCapability::from_wire`); an absent capability is treated as
sprite-only. Rust and C++ share the exact encoding
(`cbor_control_test.cpp` reproduces the `mdc-protocol` bytes).

## Scene payload (N723/N724)

`SCENE` uses CBOR `SceneCommand { action, scene? }`. `SceneNode.kind` accepts
`polygon` and `polyline` (N723); their absolute vertices live in `value2` as
`"x,y;x,y;..."` (at most 64 points, at least two), while `rect` remains the
bounding box. `arc`/`pie` (N733) read `"start,end"` degrees and `path` (N734)
reads three/four bezier points; N743 also accepts a bounded `M/L/Q/C/Z` command
stream. `mask` (N744) clips children to its polygon. `text` `mode` tokens (N740) add `ellipsis` and
`valign=top|middle|bottom`. `SceneAnim.easing` (N724) is an optional `linear`/`ease-in`/
`ease-out`/`ease-in-out` token shaping each keyframe segment. N853 adds
`SceneAnim.frame` (`[[time_ms, frame_index]...]`), a step-sampled atlas-frame
track applied only to `sprite`. N853/N854 also apply the existing `scale` and
`offset` tracks to point geometry (`polygon`, `polyline`, `mask`, path command
streams and the legacy bezier), not only to `rect`. Both fields are additive;
nodes without them behave as before.

## OTA chunk ranges (N737)

`OTA` commands are CBOR `OtaCommand { action, version?, size?, sha256? }`.
N737 adds optional `offset`/`len` (u32) for a `chunk` command; older peers omit
them and keep the whole-image behaviour. The host can compute missing ranges
with `Session::ota_missing_chunks` and emit only those on a reconnect.

## Runtime package transfer (N22)

`PACKAGE` (0x32) is a host → device message that carries an MDC runtime package
to the device. Its payload reuses the `FRAME` phase byte: `1` begin
(`[1][total_bytes u32 BE][checksum u64 BE]`), `2` chunk (`[2][offset u32 BE]
[data]`), `3` commit (`[3]`). The container bytes (see
[model-compiler.md](model-compiler.md)) are the stream being transferred, so the
begin only sizes and checks the assembly buffer; the container's own manifest
and checksum are verified after assembly.
