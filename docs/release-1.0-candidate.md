# Release 1.0 Candidate Gates

## Public API

- [x] Rust: `Session`, `DeviceManager`, `Frame`/`Tile` stable surface in `mdc-core` (sim-verified)
- [x] Node: facade matches native `devices` + `sendFrame` + `sendTile` + `pollEvents` (sim path)
- [x] C ABI: `mdc_session_create` / `destroy` / `send_frame` / `send_tile` / `device_id` / surface helpers
- [ ] No breaking CBOR field renames without protocol minor bump

## Protocol

- [x] Major-version mismatch rejected (sim-verified)
- [x] Frame legacy + chunked; Tile base check (sim-verified)
- [x] Input / OTA capability gates (sim-verified)
- [ ] Scene remains reserved; old devices Frame/Tile only
- [ ] Physical round-trip on at least one board profile

## Multi-board

- [x] Board YAML template + contribution docs
- [x] Sim board `esp32-jc3248w535-sim` baseline
- [ ] Second physical board profile with measured validation doc
- [ ] Flasher path beyond `SimFlasher` for one MCU family

## Sim exit (G03)

Host FakeDevice path + CLI `simulate` + workspace tests: **sim-verified**. Physical gates remain open for 1.0 final.
