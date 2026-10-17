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
  kind:     "group" | "rect" | "text" | "sprite" | "image" | "circle" | "line"
  rect:     [x, y, w, h]?     # geometry for kinds that rasterize (N79); a group's transform (N138)
  children: [SceneNode]?      # optional, ordered back-to-front
  value:    string?           # rect/circle/line: "#RRGGBB"/"#RGB"/name; text: glyphs; image: packed RGB565 hex
  opacity:  u8?              # N338: source alpha 0..=255 for rasterised kinds (default 255)
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
  string lives in `value`. `rect` height sets a uniform integer glyph scale
  (`h/7`, clamped to 1..=8). N318: the string is word-wrapped to `rect` width
  (`w / (6*scale)` columns, spaces are break points, a longer word is hard-broken)
  and explicit `\n` starts a new line; wrapped lines are drawn at `8*scale` px
  line height and clipped by the framebuffer. Unknown letters are skipped.
- N338: any rasterised node (`rect`/`text`/`image`/`sprite`/`circle`/`line`) may
  set `opacity` (0..=255); it is applied as source-over alpha onto whatever is
  already on the framebuffer (`255`/absent is opaque).
- `image` blits tightly packed RGB565 when `value` is `w*h` hex words (4 digits
  each, little-endian on the framebuffer). A short or malformed string draws nothing.
- `sprite` blits atlas frame `value` (a decimal index) into `rect`, nearest-neighbour.
  A missing atlas or index draws nothing.
- `circle` fills the ellipse inscribed in `rect` with the `value` colour; a  zero width or height draws nothing.
- `line` draws a Bresenham segment from `rect[0..2]` to `rect[2..4]`. A degenerate
  segment draws a single pixel; endpoints outside the framebuffer are clipped.
- `group` (and any node whose kind does not rasterize) does not draw; its `rect`
  is a transform for its children: `x`/`y` become a pixel offset and `w`/`h` a
  scale in permille (1000 = 1×; `0` leaves an axis unscaled). Nested groups
  compose. A drawable node is not treated as a group even with children.
- `render_scene` clears the framebuffer first (N137), so repeated renders are
  deterministic rather than accumulating.
- A clip `fade_in_ms` darkens the atlas frame until that many virtual milliseconds pass.
- N154: switching clips cross-fades from the frame frozen at the switch over the
  new clip's `fade_in_ms` (capped at 2 s); a device that renders no clip clears
  the frozen frame when a scene is set.
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
