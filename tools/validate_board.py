#!/usr/bin/env python3
"""Validate Board Profile fields used by MyDeviceCanvas."""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

REQUIRED = ("id:", "mcu:", "flash_bytes:", "psram_bytes:", "display:", "touch:", "ota:")
# status values that mean "schema may be complete but must not claim physical verify"
UNVERIFIED_STATUS = {"unverified", "reference-unverified"}


def scalar(text: str):
    text = text.strip().strip("\"'")
    if text in {"true", "false"}:
        return text == "true"
    if text.isdigit():
        return int(text)
    if text.startswith("0x") or text.startswith("0X"):
        try:
            return int(text, 16)
        except ValueError:
            return text
    return text


def parse_profile(path: Path) -> dict[str, object]:
    values: dict[str, object] = {}
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("-") or ":" not in line:
            raise ValueError(f"line {number}: expected key: value")
        key, value = line.split(":", 1)
        key, value = key.strip(), value.strip()
        if value.startswith("{") and value.endswith("}"):
            values[key] = "map"
            for pair in value[1:-1].split(","):
                if ":" not in pair:
                    raise ValueError(f"line {number}: invalid inline map")
                child, child_value = pair.split(":", 1)
                values[f"{key}.{child.strip()}"] = scalar(child_value)
        elif value:
            values[key] = scalar(value)
        else:
            values[key] = ""
    return values


def validate(path: Path) -> list[str]:
    values = parse_profile(path)
    errors = [
        f"missing {key[:-1]}"
        for key in REQUIRED
        if key not in {f"{k}:" for k in values} and key[:-1] not in values
    ]
    status = str(values.get("status", "")).lower()
    unknown = [
        key
        for key, value in values.items()
        if key != "status" and str(value).lower() in {"unknown", ""}
    ]
    errors.extend(f"{key} is unknown" for key in unknown)

    width, height = values.get("display.width"), values.get("display.height")
    if isinstance(width, int) and isinstance(height, int):
        if width <= 0 or height <= 0:
            errors.append("display geometry must be positive")
        if width * height * 2 > 1024 * 1024:
            errors.append("RGB565 framebuffer exceeds 1 MiB host limit")
        # Dual RGB565 budget note (active+staging)
        dual = width * height * 2 * 2
        max_fb = values.get("max_framebuffer_bytes")
        if isinstance(max_fb, int) and max_fb < dual:
            errors.append(
                f"max_framebuffer_bytes {max_fb} < dual RGB565 budget {dual}"
            )

    flash = values.get("flash_bytes")
    if isinstance(flash, int) and flash <= 0:
        errors.append("flash_bytes must be positive")

    # I2C touch + QSPI display must not claim the same pin numbers when both set
    qspi = [
        values.get(k)
        for k in (
            "pin_qspi_cs",
            "pin_qspi_clk",
            "pin_qspi_d0",
            "pin_qspi_d1",
            "pin_qspi_d2",
            "pin_qspi_d3",
        )
        if isinstance(values.get(k), int)
    ]
    touch_pins = [
        values.get(k)
        for k in ("pin_touch_sda", "pin_touch_scl", "pin_touch_int", "pin_touch_rst")
        if isinstance(values.get(k), int)
    ]
    overlap = set(qspi) & set(touch_pins)
    if overlap:
        errors.append(f"display/touch pin collision: {sorted(overlap)}")

    if status in UNVERIFIED_STATUS:
        errors.append(f"status {status}: not physically verified (reference only)")

    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description="validate an MDC board profile")
    parser.add_argument("profile", type=Path)
    args = parser.parse_args()
    try:
        errors = validate(args.profile)
    except (OSError, ValueError) as error:
        print(f"invalid: {error}", file=sys.stderr)
        return 2
    if errors:
        for error in errors:
            print(f"invalid: {error}", file=sys.stderr)
        return 1
    print(f"valid: {args.profile}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
