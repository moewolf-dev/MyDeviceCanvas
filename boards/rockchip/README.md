# Rockchip family

Skeleton board profiles for Rockchip SoC boards (N06). Reference tree:
workspace `docs/rockchip-mpp` (Media Process Platform, outside this repo).

- Status is `sim-baseline`; `physical: pending`.
- **Form B** candidates: they can render locally and mirror frames to the host
  (`mirror: true`). See `docs/render-topology-decision.md`.
- Adding a board here must not change the wire protocol.
