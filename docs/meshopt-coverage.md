# meshopt decoder coverage (N563)

`crates/mdc-model/src/meshopt.rs` is a pure-Rust port of the meshopt reference
decoder. Because a wrong decode is worse than a rejected one, the tests pin every
validation branch and every decoder path. This page is the branch inventory the
tests are written against; it is host-only logic and carries no hardware claim.

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

## Not covered (declared gaps)

- meshopt **encoding** (the inverse). No reference encoder is available to
  cross-check, so a self-written encoder cannot be verified. `compact_geometry`
  and `optimize_index_order` (see `model-compiler.md`) are the host-side,
  verifiable substitutes.
- SIMD fast paths from the C++ decoder (the port follows the pure-JS reference).
