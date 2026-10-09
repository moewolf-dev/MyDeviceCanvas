#!/usr/bin/env python3
import argparse
import re
from pathlib import Path

from validate_board import parse_profile, validate


def macro_name(board_id: str) -> str:
    return re.sub(r"[^A-Za-z0-9]", "_", board_id).upper()


def generate(profile: Path, output: Path) -> None:
    errors = validate(profile)
    if errors:
        raise ValueError("; ".join(errors))
    values = parse_profile(profile)
    name = macro_name(str(values["id"]))
    fb = values.get("max_framebuffer_bytes", values.get("display.width", 0))
    text = f"""#pragma once
// Generated from {profile.as_posix()}; do not edit.
#define MDC_BOARD_ID \"{values['id']}\"
#define MDC_BOARD_MCU \"{values['mcu']}\"
#define MDC_BOARD_FLASH_BYTES {values['flash_bytes']}u
#define MDC_BOARD_PSRAM_BYTES {values['psram_bytes']}u
#define MDC_DISPLAY_WIDTH {values['display.width']}u
#define MDC_DISPLAY_HEIGHT {values['display.height']}u
#define MDC_DISPLAY_ROTATION {values['display.rotation']}u
#define MDC_MAX_FRAMEBUFFER_BYTES {values.get('max_framebuffer_bytes', 614400)}u
#define MDC_MIN_INTERNAL_FREE_BYTES {values.get('min_internal_free_bytes', 0)}u
#define MDC_DMA_ALIGN_BYTES {values.get('dma_align_bytes', 4)}u
#define MDC_TOUCH {1 if values.get('touch') is True else 0}
#define MDC_OTA {1 if values.get('ota') is True else 0}
#define MDC_BOARD_{name} 1
"""
    _ = fb
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("profile", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    try:
        generate(args.profile, args.output)
    except (OSError, KeyError, ValueError) as error:
        parser.error(str(error))
