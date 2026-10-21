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
  kind:     "group" | "rect" | "text" | "sprite" | "image" | "circle" | "line" | "polygon" | "polyline" | "arc" | "pie" | "path" | "mask" | "gradient" | "rounded-rect" | "clip"
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
- N585/N635: an `image` with `fit` letterboxes inside its rect; `align` selects
  the placement, as a single token (`left`/`center`/`right`/`top`/`middle`/
  `bottom`) or comma-separated axes (`"right,bottom"`).
- N645: `pad=N` (in the same `mode` list) insets the fitted image by `N` pixels on
  every side; it is clamped so the inner box stays non-empty and is ignored
  without `fit`.
- N655: `pad-l=N`/`pad-t=N`/`pad-r=N`/`pad-b=N` override one side (unset sides
  keep `pad`). The image is aligned inside the remaining box, so right/bottom
  padding shrinks it without moving the content.
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
  rect: [[time_ms, x, y, w, h]...]}`. Keyframes are interpolated on the device
  clock; a looping clip wraps, otherwise it holds the last frame. N724:
  `easing` (`linear` default, `ease-in`, `ease-out`, `ease-in-out`) shapes each
  segment while keeping both endpoints exact.
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
  N665: `pad`/`pad-l/-t/-r/-b` inset the box before the fit, so a rotated image
  stays inside the padded area. N675: `align` (horizontal and `top`/`middle`/
  `bottom`) then positions the fitted image inside the padded box. A short or
  malformed string draws nothing.
- N695: `sprite` uses the same `fit`/`pad` box and applies `flip-h`/`flip-v`/quarter-turn `rotate` sampling as `image`.
- `sprite` blits atlas frame `value` (a decimal index) into `rect`, nearest-neighbour by default. N705: the `mode` token `linear` selects bilinear sampling, matching `image` (interior upscaled pixels blend instead of repeating a texel). N715: `fit`, `pad` and `linear` compose — the letterbox is inset by `pad` (per-side overrides included), the draw box keeps the atlas aspect ratio, and interior samples blend.
  N685: it shares the image's `fit`/`pad`/`align` box (aspect-preserving
  letterbox and padding). A missing atlas or index draws nothing.
- `circle` fills the ellipse inscribed in `rect` with the `value` colour; a  zero width or height draws nothing.
- `line` draws a Bresenham segment from `rect[0..2]` to `rect[2..4]`. A degenerate
  segment draws a single pixel; endpoints outside the framebuffer are clipped.
- N723: `polygon` fills a closed polygon and `polyline` strokes it. Both read
  absolute points from `value2` (`"x,y;x,y;..."`, up to 64 points, at least two);
  `rect` is the bounding box used for lint/hit-testing. `polygon` uses the
  even-odd rule and clips to the framebuffer; `polyline` joins the points with
  Bresenham segments and `mode: "closed"` also draws the last-to-first edge.
  Malformed/over-limit point lists draw nothing.
- N733: `pie` fills the ellipse sector inscribed in `rect`; `arc` strokes that
  ellipse. `value2` is `"start,end"` in degrees (clockwise, 0 = +x); a
  non-positive sweep wraps to the full circle. `value` is the colour.
- N734: `path` strokes a bezier curve from `value2` (`"x0,y0;cx,cy;x1,y1"` for
  quadratic or one more control point for cubic), flattened into 32 segments.
  Three/four points only; other counts draw nothing. N743: a command stream
  instead (`"M1,1;L5,1;Q8,4 5,8;C1,1 2,2 3,3;Z"`, ≤64 commands, must start with
  `M`); `M/L/Q/C/Z` are supported and `Z` closes to the start. N753 adds
  `A rx,ry,rotation,large-arc,sweep,x,y` (elliptical arc, flags 0/1, radii taken
  as magnitudes) and `T x,y` (smooth quadratic, reflecting the previous control).
  N763 adds lowercase relative commands (`m/l/q/c/s/t/a`) whose coordinates are
  offsets from the current point and may be mixed with absolute ones, plus
  `S cx,cy,x,y` (smooth cubic, reflecting the previous cubic's second control).
  `Z` returns the current point to the subpath start; relative accumulation
  saturates instead of wrapping.
- N744: `mask` composites its children only inside the polygon from `value2`
  (same point syntax as `polygon`); children keep absolute coordinates and the
  node is clipped to the framebuffer. A missing/short polygon draws nothing.
  N754: `mode` may add `invert` (keep the outside) and `feather=N` (1..8 px box
  blur on the boundary; the child is alpha-blended at the soft edge).
- N740: `text` `mode` accepts `ellipsis` (cut over-long lines to the rect width
  and append `...`) and `valign=top|middle|bottom` (place the glyph block inside
  the rect height). Both are comma-separated with other tokens. N751 adds
  `bg=#RRGGBB` (fill each line box) and `underline` (stroke below the glyphs).
- `group` (and any node whose kind does not rasterize) does not draw; its `rect`
  is a transform for its children: `x`/`y` become a pixel offset and `w`/`h` a
  scale in permille (1000 = 1×; `0` leaves an axis unscaled). Nested groups
  compose. A drawable node is not treated as a group even with children.
  N759: `value2` on a group is a shear `"kx,ky"` in permille (`x += kx·y`,
  `y += ky·x`; clamped to ±2000). A sheared `rect` is rasterized as a
  parallelogram; other raster kinds use the sheared bounding box, while point
  geometry (polygon/polyline/path) is sheared exactly. Malformed shear is
  ignored. N768: hit-testing inverts the affine matrix and tests the local rect,
  so pixels in the empty corners of the bounding box are not hits.
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
