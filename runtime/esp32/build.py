#!/usr/bin/env python3
"""Validate a board and generate its compile-time constants before ESP-IDF."""
import argparse
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).parents[2] / "tools"))
from generate_board_header import generate

parser = argparse.ArgumentParser()
parser.add_argument("profile", type=Path)
parser.add_argument("--out", type=Path, required=True)
args = parser.parse_args()
generate(args.profile, args.out / "mdc_board_config.h")
print(f"generated {args.out / 'mdc_board_config.h'}")
