# Scene Format (v1)

Status: **sim-verified** (host logic); physical delivery is `physical: pending`.
This is the L01 vector-scene contract. A device that does not implement Scene
keeps using Frame/Tile only.

## Capability

`CAPABILITIES.scene { max_nodes, max_depth }`. `max_nodes == 0` (the default)
means the device does not implement Scene; the field is omitted on the wire so
pre-scene peers decode unchanged. A host must reject a scene that exceeds the
advertised limits before sending.

## Message

`SCENE` (0x34) carries a CBOR `SceneCommand`:

```text
SceneCommand:
  action: "set" | "clear"
  scene:  SceneNode?          # required for "set"

SceneNode:
  name:     string
  kind:     "rect" | "text" | "sprite" | "image"
  children: [SceneNode]       # optional, ordered back-to-front
  value:    string?           # kind-specific (text payload, sprite ref)
```

Canonical bytes: `protocol/vectors/scene_command.json`.

## Rules

1. A host may only send a `set` after the device advertised scene support and
   the node count/depth fit `max_nodes`/`max_depth`.
2. `clear` removes the current scene; the device keeps presenting the last
   Frame/Tile until a new scene or frame arrives.
3. Scene is a *structure* message only: it does not define a full renderer or
   animation; a device with a GPU may render it, a weak MCU may refuse.
4. Unknown `kind` values are rejected by the device rather than ignored.
5. Scene never changes the Frame/Tile path; a scene-unaware device is unaffected.

## Non-goals

- No full Scene runtime, shader, or 3D path in this wave.
- No host-side rendering; the host only describes structure.
- No physical device required; D05/G03 remain open.
