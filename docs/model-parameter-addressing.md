# Model parameter addressing (CORE-R11 design)

Status: **design only**; no protocol or device behavior is implemented yet.
This document must be reviewed before implementation. It is the Form B
(device-rendered, host-mirrored) counterpart of the App's parameter tree.

## 1. Goal and non-goals

Goal: a parameter change in the App must change the **device-rendered** frame,
not only the host preview. The App already derives stable ids from the source
glTF; Core must accept a bounded update addressed by the same ids and apply it to
the installed package before the next device render. A new package replaces the
old parameter state atomically.

Non-goals:

- No `.moc3`, new animation language, shader graph or arbitrary host script.
- No per-frame streaming; updates are explicit, bounded and coalesced.
- No change to Form A. A device without the capability keeps today's behavior.
- No security claim: the package identity is a staleness check, not a signature.

## 2. Existing pieces this builds on

- `mdc-model::DevicePackage` keeps node/material names plus translations,
  rotations, scales, morph targets/weights and material colours; the compiler
  already produces mesh and sprite packages.
- `mdc-bake::render_package_scene_clip_skinned` samples animations, node world
  matrices, morph weights and material colours for the device render.
- The simulator keeps the accepted `DevicePackage`, renders it at the device
  clock and emits `MIRRORFRAME`.
- `mdc-protocol` has free type space after `Scene (0x34)`; `Capabilities` has an
  optional `renderer` struct.
- The App's ids are `node/<name>/tx|ty|tz|sx|sy|sz|ry`,
  `material/<name>/color|metallic|roughness`, `morph/<node>/<index>`
  (URL-encoded name segments, bounded by the App's import limits).

## 3. Package identity

`PackageManifest` already has a 64-bit FNV-1a `checksum`, `body_bytes` and
`version`. There is no SHA-256 in the device runtime, so:

- `package_id = manifest.checksum` (u64)
- `body_bytes = manifest.body_bytes` (u32)
- `generation` = device counter incremented on every successful package install
  (and reset on reboot)

An update carries all three. A mismatch is rejected with `E_STALE_PACKAGE` and
applies nothing. Installing a package clears the parameter map. The parameter
state is not persisted across reboot.

This is deliberately not a cryptographic identity; firmware/package signatures
remain the trust mechanism.

## 4. Wire message

New Host→Device type `MODELPARAMS = 0x35` (CBOR control payload, 4 KiB cap):

```cddl
model_params = {
  package_id: uint,        ; manifest checksum (u64)
  body_bytes: uint,        ; manifest body length
  generation: uint,        ; install generation
  params: [* param],
}
param = {
  id: tstr,                ; <= 128 bytes, App-compatible
  f32: float32,            ; node trs / morph / pbr
} / {
  id: tstr,
  rgb565: uint,            ; material/color
}
```

Rules:

- At most 64 params per message; duplicate ids are last-wins in the message.
- `id` is percent-encoded ASCII (the App's namespace); cap at 128 bytes.
- Unknown ids are rejected per item and counted; known items in the same message
  still apply. The response reports `rejected`.
- Value checks: finite f32; scale > 0; morph 0..1; rotation -180..180 degrees;
  rgb565 <= 0xFFFF.
- Idempotent: applying the same update twice yields the same frame.
- Whole-message rejection (bad package identity, too many params, oversized id)
  uses the existing `ERROR`; otherwise `ACK(received=true)`.
- No `displayed` flag for params: the next `MIRRORFRAME` is the visual evidence.

Old devices: `MessageType::try_from(0x35)` fails and the host only sends after a
capability negotiation, so a non-supporting device never sees the message.

## 5. Capability

Extend `mdc-protocol::RendererCapability` (additive; omitted when absent):

```cddl
params = {
  max_per_update: uint,   ; 0 = unsupported
  max_id_bytes: uint,     ; e.g. 128
  max_pending: uint,      ; device coalescing map, e.g. 64
  max_fps: uint,          ; accepted update rate
  kinds: [* tstr],        ; "node_trs" | "material_color" | "material_pbr" | "morph"
}
```

A device advertises only what its backend applies:

- mesh backend: `node_trs`, `material_color`, `morph` (PBR only if it changes
  pixels; test it)
- sprite backend: no node/material/morph claims in this batch
- `max_per_update == 0` → App keeps those parameters on Form A

The host intersects its own limit with the device's; no board-name branching.

## 6. Device model

```
ParamState {
  package_id, body_bytes, generation,
  pending: BoundedMap<ParamId, ParamValue>,  // max_pending, last-wins
  applied: BoundedMap<ParamId, ParamValue>,
  last_update_ms, rejected_count,
}
```

Update flow:

1. Validate identity, count, id length and value ranges.
2. Insert into `pending`; if the map is full, reject the whole update (do not
   silently drop an unrelated parameter).
3. At the next device-clock render tick, promote `pending` into an immutable
   override set and clear `pending`. No render happens mid-update.

Renderer integration (mesh backend): add `ParamOverrides` to
`render_package_scene_clip_skinned` and the static variants:

- `node_trs: Map<usize, TrsOverride>` applied after animation sampling
  (translation/scale replace; `ry` composes yaw).
- `material_color: Map<usize, [u8;3]>` replaces the material colour for the draw.
- `morph: Map<(usize, usize), f32>` replaces the animation/default weight.

The override set is bounded and immutable during a render; no package clone per
frame.

## 7. SDK and App

Node SDK:

```ts
const installed = await surface.installPackageEx(bytes)
// { ready: boolean, packageId: number, bodyBytes: number, generation: number }

await surface.setModelParams({
  packageId: installed.packageId,
  bodyBytes: installed.bodyBytes,
  generation: installed.generation,
  params: [{ id: "node/head/ty", f32: 0.12 }]
})
// -> { accepted, rejected, generation }
```

- Keep `installPackage` returning `boolean` for compatibility; add
  `installPackageEx` (or `packageIdentity()`) for the identity.
- The SDK validates/batches to `max_per_update`, keeps the last update for
  retries and returns the rejected count.
- App maps its ids 1:1; a parameter whose kind is not advertised stays Form A
  and is labelled as such. Send one update per user gesture, throttled to
  `max_fps`, not per animation frame.

## 8. Budgets

| Item | Limit |
|---|---|
| control payload | 4 KiB (existing) |
| params per message | 64 |
| id bytes | 128 |
| pending map | 64 entries |
| update rate | device `max_fps`, default 30 |
| path segments | 3 (`node/name/tx`) |
| value types | f32, rgb565 u16 |
| stale package | reject, no apply |

## 9. Tests

- `MP-T01` codec: Rust encode/decode + C++ `0x35` parser; unknown type on old
  peers; >4 KiB rejected before allocation.
- `MP-T02` node TRS: install mesh package, set `node/<name>/ty`, render+mirror,
  assert the expected pixel/bbox moved.
- `MP-T03` material colour: change `material/<name>/color`, assert the sampled
  pixel colour changes.
- `MP-T04` morph: set a weight, assert geometry changes; 0 restores.
- `MP-T05` stale identity: old checksum/body/generation rejected, frame hash
  unchanged.
- `MP-T06` budgets: 65 params, 129-byte id, NaN/Inf, bad colour/morph rejected
  without panic; pending map bounded.
- `MP-T07` idempotence: same update twice → same frame hash.
- `MP-T08` capability: no `params` → SDK reports Unsupported; App stays Form A.
- `MP-T09` App E2E: slider → Core update → mirror pixels change; no Form A frame
  for that parameter.

## 10. Task split

- **CORE-R11a (batch 1)**: `0x35` + capability + Rust/C++ vectors; simulator
  `ParamState`; node TRS + material colour overrides; MP-T01/02/03/05/06/07;
  Node `installPackageEx`/`setModelParams`; App maps node position and colour.
- **CORE-R11b**: morph + PBR (only if it changes pixels); batching/rate limit;
  MP-T04/08; App labels and limits.
- **CORE-R11c**: Linux runtime parity; sprite backend decision; MP-T09; fresh
  60-minute soak + rc.

## 11. Open questions

- Full manifest hash vs FNV checksum for package id: FNV is what the device
  already computes; use it unless a collision matters.
- Are node names unique after URL encoding? The compiler must expose the exact
  ids it accepted; the App must not re-derive them differently.
- Does the device need a parameter reset message, or are explicit defaults
  enough? Prefer explicit defaults in the same `model_params` shape.
