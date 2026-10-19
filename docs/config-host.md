# Configuration Host (N39–N48)

Status: **sim-verified** (host logic); physical delivery is `physical: pending`.

The Mac app is the Configuration Host: it manages device settings (brightness,
rotation, Wi-Fi, character). This is the third pillar alongside the Model
Compiler and the Mirror/OBS bridge. Settings travel over the `CONFIG` control
message, gated by the device's advertised keys.

## Protocol

`CAPABILITIES.config { keys: [string] }` lists the keys a device accepts. It is
optional on the wire: an empty/absent `config` is omitted, so pre-config peers
see an unchanged capabilities map. `CONFIG` (0x33) carries a CBOR
`ConfigCommand { action: get|set, key, value? }`; the device answers with a CBOR
`ConfigReply { key, ok, value?, error? }`. Canonical bytes are recorded in
`protocol/vectors/config_command.json`.

## Host API

- `Session::set_config(key, value)` / `get_config(key)` — gated on
  `Capabilities.config.supports(key)` and `MAX_CONFIG_VALUE_BYTES` (256).
- `Session::config(key)` / `config_values()` — the values learned from replies.
- `mdc config get <key>` / `mdc config set <key> <value>` (CLI, sim).
- Node `getConfig` / `setConfig`; C ABI and Swift use the same Core path.
- The reserved `animation` key plays a baked-atlas clip (see
  [baked-sprites.md](baked-sprites.md)); it is handled by the device runtime, not
  the settings map, and is refused when no atlas is installed.

## Guarantees

- **Capability gate**: an unadvertised key is refused host-side with
  `Unsupported`; nothing is sent.
- **Bounded**: values over `MAX_CONFIG_VALUE_BYTES` are refused before any
  packet; the device validates per key (brightness 0–255, rotation 0/90/180/270,
  Wi-Fi length, `tile_missing_threshold` integer) and rejects invalid values.
- **Negotiated behaviour**: `tile_missing_threshold` (N586) is a config key the
  host reads to decide how many unacknowledged tiles it tolerates before forcing
  a full frame (`Session::enforce_tile_reconciliation_from_config`). N596: each
  board carries its own `BoardProfile.config_defaults`, so the default is a board
  fact (the simulator's `linux-virt` ships `2`; the baseline ships `0`). `mdc
  inspect` reports the board's `config_defaults` (N606).
- **Redaction**: a password/token value is never echoed in a reply — the device
  answers `***` on `get`, and the CLI/Node surface shows the redacted value. The
  stored value never enters a log.
- **No silent overwrite on failure**: a rejected `set` leaves the last known
  value in the host cache.

## Non-goals

- No arbitrary/unknown keys: the device decides which keys it accepts.
- No Wi-Fi credential transport in this wave beyond the redacted key path; real
  pairing/credential handoff stays in `mdc-discovery`/`mdc-provision`.
- No physical device is required; D05/G03 remain open.
