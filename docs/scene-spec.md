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
  kind:     "group" | "rect" | "text" | "sprite" | "image" | "circle" | "line" | "gradient" | "rounded-rect" | "clip"
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
- N338: any rasterised node (`rect`/`text`/`image`/`sprite`/`circle`/`line`/
  `gradient`) may set `opacity` (0..=255); it is applied as source-over alpha onto
  whatever is already on the framebuffer (`255`/absent is opaque).
- N348: `gradient` fills `rect` with a vertical linear gradient from `value` (top)
  to `value2` (bottom; a missing `value2` is a flat fill).
- N515: `mode: "radial"` makes a `gradient` interpolate by distance from the
  rect centre (the start stop is the centre); the default is a linear ramp.
- N525: a linear `gradient` follows `rotate` (0 = top-to-bottom, 90 =
  left-to-right, 180/270 reverse, any other angle is a diagonal).
- N535: a radial `gradient` accepts `focus: "cx%,cy%"` (default `50,50`) to move
  its centre within the rect.
- N505: `gradient` accepts a comma-separated stop list in `value2`
  (`0:#FF0000,50:#00FF00,100:#0000FF`), interpolating between stops; a value
  without a comma keeps the two-stop `value`/`value2` ramp.
- N355: `rounded-rect` fills `rect` with the `value` colour and rounded corners;
  `value2` is the corner radius in pixels (clamped to half the smaller side).
- N368: `clip` treats `rect` as a scissor: its children are offset by the rect's
  origin (not scaled) but only pixels inside the rect are composited, so children
  extending past it are cut off. Nesting is supported.
- N450: `visible: false` hides a node and its children from drawing and hit-testing
  (`true`/absent is visible).
- N463: `anim` is `{duration_ms, looping, opacity: [[time_ms, opacity]...],
  rect: [[time_ms, x, y, w, h]...]}`. Keyframes are linearly interpolated on the
  device clock; a looping clip wraps, otherwise it holds the last frame.
- N546: `anim.focus: [[time_ms, cx%, cy%]...]` animates the radial `gradient`
  centre (same percent semantics as `focus`); values are clamped to `0..=100`.
- N475: `rotate: 90|180|270` swaps the rect axes for axis-aligned fills; a rotated
  `gradient` runs horizontally.
- N480: `blend: "additive"` adds a `rect`/`gradient` to the framebuffer (saturating
  per channel) instead of covering it; the default is `source-over`.
- N484: `align: "left" | "center" | "right"` positions each `text` line within the
  rect width.
- N485: `z` sets a sibling's draw order (higher draws on top; ties keep document
  order). Hit-testing searches the highest `z` first.
- N495: `wrap: "word" | "char" | "none"` controls `text` line breaking (the
  default is word wrapping).
- N497: a `line` with `value2` set to a dash length (in pixels) is drawn dashed;
  `value2` unset (or `0`) draws it solid.
- N484/N485/N495: `align`, `z`, and `wrap` are documented above.
- N493: a recognised gesture carries the hit-tested scene node when the device
  reported one on the input event.
- `image` blits tightly packed RGB565 when `value` is `sw*sh` hex words (4 digits
  each, little-endian on the framebuffer). `value2: "sw,sh"` sets the source grid
  (default: the rect size) and `mode` is a comma list of `linear` (bilinear
  filtering; nearest is the default), `flip-h`, `flip-v` and `fit`. N565: the
  node's `rotate` (0/90/180/270) turns the sampled image about the rect centre.
  N575: `fit` preserves the source aspect ratio (letterboxed in the rect) instead
  of stretching each axis independently. N585: with `fit`, `align`
  (`left`/`center`/`right`, default `center`) positions the letterbox
  horizontally.   N605: a 90°/270° `rotate` swaps the fitted box's width/height so
  the rotated image keeps its aspect. N625: the image uses its **authored** rect
  for the fit box (the axis swap the other fills use would double-rotate it), and
  `align` positions the box. A short or malformed string draws nothing.
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
