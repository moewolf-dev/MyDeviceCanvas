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
  kind:     "group" | "rect" | "text" | "sprite" | "image"
  rect:     [x, y, w, h]?     # geometry for kinds that rasterize (N79); a group's transform (N138)
  children: [SceneNode]?      # optional, ordered back-to-front
  value:    string?           # rect: "#RRGGBB"/"#RGB"/name; text: glyphs; image: packed RGB565 hex
```

Canonical bytes: `protocol/vectors/scene_command.json` (no geometry) and
`scene_rect.json` (a rect with geometry + colour).

## Rendering (N80+)

A device that accepts a scene rasterizes it locally:

- Nodes are visited depth-first; a parent is drawn before its children, so
  children appear on top (z order = document order).
- `rect` nodes fill their `rect` with the colour from `value`. `#RRGGBB`,
  `#RGB`, bare hex and a small set of CSS names (`red`, `green`, `blue`,
  `black`, `white`, `yellow`, `cyan`, `magenta`, `orange`, `gray`) are accepted;
  missing/malformed → opaque white. Geometry is clipped to the framebuffer; an
  out-of-bounds rect draws its in-bounds part and never panics.
- `text` draws a 5×7 bitmap for space, digits, `A–Z` (lowercase maps to its
  uppercase glyph) and punctuation `.,:!?-+/()=*#`, starting at `rect.x/y`. The
  string lives in `value`. `rect` width/height set an integer glyph scale
  (`w/6`, `h/7`, each clamped to 1..=8). Unknown letters are skipped.
- `image` blits tightly packed RGB565 when `value` is `w*h` hex words (4 digits
  each, little-endian on the framebuffer). A short or malformed string draws nothing.
- `sprite` blits atlas frame `value` (a decimal index) into `rect`, nearest-neighbour.
  A missing atlas or index draws nothing.
- `group` (and any node whose kind does not rasterize) does not draw; its `rect`
  is a transform for its children: `x`/`y` become a pixel offset and `w`/`h` a
  scale in permille (1000 = 1×; `0` leaves an axis unscaled). Nested groups
  compose. A drawable node is not treated as a group even with children.
- `render_scene` clears the framebuffer first (N137), so repeated renders are
  deterministic rather than accumulating.
- A clip `fade_in_ms` darkens the atlas frame until that many virtual milliseconds pass.
- When the device mirrors, the rasterized scene is emitted as a `MIRRORFRAME`.
  Setting a scene clears any running animation clip (last-wins).

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
