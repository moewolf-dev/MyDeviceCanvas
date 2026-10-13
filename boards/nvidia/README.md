# NVIDIA Jetson family

Skeleton board profiles for NVIDIA Jetson boards (N06). Reference tree:
`docs/reference/nvidia-cuda-samples` (3D rendering path reference).

- Status is `sim-baseline`; `physical: pending`.
- **Form B** candidates: local render + mirror. The high-end 3D path is a
  `mdc-model` target-backend concern (`docs/render-topology-decision.md` §4–§5).
- Adding a board here must not change the wire protocol.
