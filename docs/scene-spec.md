# Scene Capability (Reserved)

## Intent

A future **Scene** message family may describe multi-layer / declarative layouts. For protocol v1.0 host and devices:

- Capability bit / flag for Scene is **reserved** (not negotiated in Capabilities CBOR yet).
- Existing devices remain **Frame** and **Tile** only.
- Hosts must not require Scene to render; absence of Scene is not an error.

## Compatibility

| Device era | Required | Optional |
|---|---|---|
| v1 Frame/Tile | Frame | Tile, Input, OTA |
| Future Scene | Frame | Scene (when advertised) |

Until a minor protocol bump documents the Scene payload, treat any unknown message type as ignorable on the device. The host API `Session::send_scene` and `mdc_session_send_scene` return unsupported and do not write a Scene packet. Old devices stay on Frame and Tile.
