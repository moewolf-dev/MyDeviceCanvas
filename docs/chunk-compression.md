# Chunk compression (N757 / N766 / N776 / N786 / N796)

Stored asset chunks are compressed per chunk (chunk boundaries are fixed by the
`ChunkStore`), so identical byte ranges still dedupe by their SHA-256 of the
**raw** bytes. The on-disk/in-memory form is:

```text
MRCH | tag | payload
tag 0: raw bytes
tag 1: RLE stream ("MRLE" + u32 length + PackBits-style packets)
tag 2: LZSS stream ("MRLZ" + u32 length + control bytes/matches)
```

`pack_chunk` computes all three and keeps the shortest payload. A legacy chunk
file without the `MRCH` magic is returned verbatim, so old stores stay readable.
The quota (`ChunkStore::total_bytes`, `DiskChunkStore::total_bytes`) counts the
**stored** bytes, i.e. after compression.

## RLE format

PackBits style: a control byte `< 128` is a literal run of `control + 1` bytes;
`>= 128` is a repeat of `257 - control` copies of the next byte. The declared
length is checked on decode. RLE is chosen for long single-byte runs.

## LZSS format

- 4 KiB window, matches of 3..=255 bytes.
- One control byte covers up to eight items (bit 0 = first item). `0` = literal
  byte; `1` = `u16` little-endian distance + `u8` length.
- Encoding is deterministic. N786: each 3-byte key keeps a chain of up to 64
  earlier positions (newest first). The encoder takes the longest in-window
  match and, on a tie, the nearer distance. The current index is linked before
  the next search, and so are the interior positions of a match. N796: if the
  next byte starts a strictly longer match, the encoder emits one literal and
  takes that longer match on the following step. The bitstream
  (magic, control byte, `u16` distance, `u8` length) and `MAX_LZ_DECODE_BYTES`
  are unchanged.
- `MAX_LZ_DECODE_BYTES` (16 MiB) is checked against the declared length **before**
  allocating the output buffer. Every match is validated against already-produced
  bytes and against the declared length, so a decompression bomb or corrupt
  stream returns `None` instead of growing without bound.

## Measured ratios (host, release-independent algorithm)

These are the deterministic fixtures asserted by
`crates/mdc-assets` (`lz_compression_ratios_are_documented`); ratios include the
container header and control overhead.

| Fixture | Input bytes | LZ output bytes | Ratio |
|---|---:|---:|---:|
| Repeated English sentence | 2,880 | 95 | 0.033 |
| Constant `0x5A` | 4,096 | 63 | 0.015 |
| xorshift binary stream | 1,024 | 1,160 | 1.133 |

For incompressible data the worst case is the literal path: 1 control byte per 8
literal bytes (12.5% overhead) plus the 8-byte container header, hence ~1.13×.
`pack_chunk` keeps the raw form when compression would be larger, so no stored
chunk is worse than `raw + 5-byte chunk header`.

## Failure and fuzz coverage (N776)

`lz_decoder_survives_a_mutation_corpus` runs every prefix (truncation) of valid
streams plus single-byte XOR mutations (`0x01`, `0x80`, `0xFF`) over text, binary,
constant and empty inputs. The decoder must return `Some` or `None` without
panicking; a declared size above the budget is rejected before allocation.
