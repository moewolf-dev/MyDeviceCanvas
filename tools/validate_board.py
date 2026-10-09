#!/usr/bin/env python3
"""Validate the deliberately small Board Profile subset used by 0.1."""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

REQUIRED = ("id:", "mcu:", "flash_bytes:", "psram_bytes:", "display:", "touch:", "ota:")


def scalar(text: str):
    text = text.strip().strip('"\'')
    if text in {"true", "false"}:
        return text == "true"
    if text.isdigit():
        return int(text)
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
    errors = [f"missing {key[:-1]}" for key in REQUIRED if key not in {f"{k}:" for k in values} and key[:-1] not in values]
    unknown = [key for key, value in values.items() if str(value).lower() in {"unknown", ""}]
    errors.extend(f"{key} is unknown" for key in unknown)
    width, height = values.get("display.width"), values.get("display.height")
    if isinstance(width, int) and isinstance(height, int):
        if width <= 0 or height <= 0:
            errors.append("display geometry must be positive")
        if width * height * 2 > 1024 * 1024:
            errors.append("RGB565 framebuffer exceeds 1 MiB host limit")
    flash = values.get("flash_bytes")
    if isinstance(flash, int) and flash <= 0:
        errors.append("flash_bytes must be positive")
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
