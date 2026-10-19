# meshopt decoder coverage (N563)

`crates/mdc-model/src/meshopt.rs` is a pure-Rust port of the meshopt reference
decoder. Because a wrong decode is worse than a rejected one, the tests pin every
validation branch and every decoder path. This page is the branch inventory the
tests are written against; it is host-only logic and carries no hardware claim.

## Round trips beyond the golden vectors

The reference fixtures do not exercise every encoder choice, so the module also
carries a small in-test encoder used for round trips:

- N623: v1 control modes 0/1/2/3 round trip back to the source values.
- N633: a control-mode-1 lane encoded with **4-bit sentinel deltas** (nibble
  literals plus `0xf` escapes) round trips, covering the `mode_bits == 4` path
  under a non-zero control mode.
- N613/N623: v0/v1 vertex round trips and index-buffer/sequence round trips.

## Success paths (golden reference vectors)

The module tests run the reference decoder's fixtures, so the branches below are
checked against expected output, not a self-consistent pair:

| Path | Test |
|---|---|
| vertex buffer `0xa0` (v0), `0xa1` (v1) | `decode_vertex_buffer_matches_the_reference_vector`, `..._v1_...` |
| v1 control modes: bit-xor/rotate, custom deltas, raw block | `..._v1_bit_xor_rotate`, `..._v1_deltas`, `..._v1_custom` |
| filters: OCTAHEDRAL / QUATERNION / EXPONENTIAL / COLOR (8- and 16-bit) | `decode_filter_*` |
| index buffer `0xe0` (v0) / `0xe1` (v1) | `decode_index_buffer_v0_16/32`, `..._v1_...` |
| index sequences `0xd0`/`0xd1` | `decode_index_sequence_matches_the_reference_vector` |

## Validation branches (directed corpus)

`every_decoder_guard_reports_a_meshopt_error` walks each guard and asserts a
`meshopt:` error, so no guard silently returns a wrong default:

- vertex header not `0xa0`/`0xa1`; stride `0`, `> 256`, or not a multiple of 4;
  truncated tail; declared size over `MAX_VERTEX_DECODE_BYTES`;
- octahedral / quaternion / exponential / color filter with the wrong stride;
- index header not `0xe0`/`0xe1`; version `> 1`; count not a multiple of 3;
  stride not 2/4; stream too short; 16-bit index overflow;
- index sequence header not `0xd0`/`0xd1`; version `> 1`; stride not 2/4;
  stream too short; LEB128 overflow.

## Fuzz passes

- `truncated_and_random_streams_never_panic`: every prefix of a set of seeds plus
  a deterministic LCG stream, across all decoders and filters.
- `fuzz_corpus_with_byte_mutations_never_panic`: `FUZZ_CORPUS` (vertex v0/v1,
  index buffer, index sequence, all-ones header, oversized LEB128) with every
  single-byte mutation (`0x00`, `0xFF`, MSB flip) and every truncation.
- `color_filter_with_zero_alpha_does_not_panic`: the specific underflow (N543).
- `vertex_buffer_round_trips` (N593): a test-only v1 raw-delta encoder proves
  `decode(encode(values)) == values` for strides 4/8/12 (multiple blocks).
- `vertex_round_trip_composes_with_filters` (N603): decoding an encoded buffer
  with a filter equals applying that filter to the raw values, for every filter.
- `vertex_buffer_v0_round_trips` (N613): the v0 raw-delta path round-trips,
  including a partial trailing 16-element group. This also pinned a **bounds
  fix**: a partial group's 16-entry write is now bounded to the delta buffer
  instead of over-running it.
- `vertex_buffer_v1_control_modes_round_trip` (N623): the v1 control modes 0/1/2/3
  (per-lane header / 1-bit / skip / raw) all round-trip. Note control mode 1 indexes
  `header_modes[2]` (its group mode 0 is the 1-bit mode), so the encoder writes the
  packed group bytes for that lane.
- `malformed_leb128_and_half_packets_are_errors` (N573): directed LEB128 overflow,
  a never-terminating LEB128 run, a value truncated mid-field, a short aux table,
  and a stream shorter than its vertex tail.

## Not covered (declared gaps)

- meshopt **encoding** (the inverse). No reference encoder is available to
  cross-check, so a self-written encoder cannot be verified. `compact_geometry`
  and `optimize_index_order` (see `model-compiler.md`) are the host-side,
  verifiable substitutes.
- SIMD fast paths from the C++ decoder (the port follows the pure-JS reference).
