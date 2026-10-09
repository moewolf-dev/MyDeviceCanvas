# Compatibility Decisions

- Protocol major mismatch is a hard error; minor versions negotiate capabilities, not behavior by version number.
- RGB565 little-endian, tightly packed rows is the only current display format.
- A surface has at most one in-flight frame and one pending latest frame.
- Host frame payloads are capped at 1 MiB and all geometry uses checked arithmetic.
- The runtime must report insufficient internal heap, PSRAM or DMA memory instead of silently falling back.
- Default acceptance is `sim-verified` against `esp32-jc3248w535-sim` and `linux-virt`.
- Physical JC3248W535 remains `physical: pending`; sim profile pins come from AgentDeck reference and are not claimed as measured on a purchased unit.
- Discovery endpoints are candidates only; HELLO confirms identity.
- Touch / OTA require capability bits; otherwise peers return Unsupported.
