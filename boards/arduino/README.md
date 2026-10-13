# Arduino family

Skeleton board profiles for the Arduino MCU class (N06). Reference tree:
`docs/reference/arduino-core-avr`.

- Status is `sim-baseline`; `physical: pending` until a real board is measured.
- These are **Form A** devices (Host render → push frames). They do not
  advertise a mirror capability.
- Adding a board here must not change the wire protocol; mode selection is
  Capability-driven.
