# Arduino family

Skeleton board profiles for the Arduino MCU class (N06). Reference tree:
workspace `docs/arduino-core-avr` (outside this repo).

- `uno-r4-ili9341` (Renesas RA4M1) and `uno-tft` (AVR ATmega328P) — the AVR
  profile's clocks/flash are grounded in `boards.txt` (N15).

- Status is `sim-baseline`; `physical: pending` until a real board is measured.
- These are **Form A** devices (Host render → push frames). They do not
  advertise a mirror capability.
- Adding a board here must not change the wire protocol; mode selection is
  Capability-driven.
