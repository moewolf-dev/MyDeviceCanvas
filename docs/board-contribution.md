# Board Contribution Guide

## Steps

1. Copy `boards/templates/board.yaml.example` to `boards/<vendor>/<board-id>/board.yaml`.
2. Fill MCU, flash, display geometry, and capability flags (`touch`, `ota`).
3. Set `status: unverified` until host sim and/or physical checks exist.
4. For simulator-only baselines (no hardware in hand), use `status: sim-baseline` and document the upstream source.
5. Add a short note under `docs/hardware-validation/` when you run FakeDevice or physical tests.
6. Never mark `physical-verified` from host-only or simulator results.

## Validation

- **Sim**: `BoardProfile` dims should match `display.width` / `display.height`; CLI `--board <id>` maps known ids.
- **Physical**: measure free heap / FPS on device; record in hardware-validation docs.

## Protocol

Old devices stay Frame/Tile only. Do not require Scene capability bits for baseline boards (see `docs/scene-spec.md`).
