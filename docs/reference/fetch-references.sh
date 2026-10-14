#!/usr/bin/env bash
# N02 — read-only reference snapshots for board / transport compatibility work.
#
# Snapshots are NOT part of this project. They live in the workspace `docs/`
# tree, next to PLAN.md, AgentDeck-master and frameos-main (N02 correction
# 2026-10-14). Each upstream project lands in its own subdirectory there.
# The tracked metadata stays here in docs/reference/references.lock.
#
# Usage: docs/reference/fetch-references.sh [name ...]
#        (no arguments clones every entry)
#        MDC_REFS_ROOT overrides the snapshot root (default: workspace docs/)
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
LOCK="$HERE/references.lock"
# Workspace docs/ == this repo's docs/../.. /docs (references are not our code).
REFS_ROOT="${MDC_REFS_ROOT:-$HERE/../../../docs}"

# name|url|tag-or-branch ("default" = upstream default branch)
REPOS=(
  "esp-idf|https://github.com/espressif/esp-idf.git|v5.3.2"
  "esp-bsp|https://github.com/espressif/esp-bsp.git|default"
  "raspberrypi-userland|https://github.com/raspberrypi/userland.git|default"
  "arduino-core-avr|https://github.com/arduino/ArduinoCore-avr.git|default"
  "rockchip-mpp|https://github.com/rockchip-linux/mpp.git|default"
  "nvidia-cuda-samples|https://github.com/NVIDIA/cuda-samples.git|default"
  "mdns|https://github.com/mjansson/mdns.git|default"
  "websocket-ws|https://github.com/websockets/ws.git|default"
  "libusb|https://github.com/libusb/libusb.git|default"
  "nimble|https://github.com/apache/mynewt-nimble.git|default"
)

wanted() {
  local needle="$1"
  shift
  [[ $# -eq 0 ]] && return 0
  for w in "$@"; do [[ "$w" == "$needle" ]] && return 0; done
  return 1
}

: >"$LOCK.tmp"
echo -e "name\turl\tref\tcommit\tlicense" >>"$LOCK.tmp"
status=0
while IFS='|' read -r name url ref; do
  if ! wanted "$name" "$@"; then continue; fi
  dest="$REFS_ROOT/$name"
  echo "== $name ($ref)"
  if [[ -d "$dest/.git" ]]; then
    git -C "$dest" fetch --depth 1 origin >/dev/null 2>&1 || true
    git -C "$dest" checkout --detach FETCH_HEAD >/dev/null 2>&1 || true
  else
    rm -rf "$dest"
    if [[ "$ref" == "default" ]]; then
      git clone --depth 1 --single-branch "$url" "$dest" || { status=1; rm -rf "$dest"; continue; }
    else
      git clone --depth 1 --branch "$ref" --single-branch "$url" "$dest" || { status=1; rm -rf "$dest"; continue; }
    fi
  fi
  commit="$(git -C "$dest" rev-parse HEAD)"
  branch="$(git -C "$dest" symbolic-ref -q --short refs/remotes/origin/HEAD 2>/dev/null | sed 's#^origin/##' || true)"
  [[ -z "$branch" ]] && branch="$ref"
  case "$name" in
    esp-idf|esp-bsp|nimble) license="Apache-2.0" ;;
    rockchip-mpp) license="Apache-2.0 OR MIT" ;;
    raspberrypi-userland|nvidia-cuda-samples) license="BSD-3-Clause" ;;
    arduino-core-avr|libusb) license="LGPL-2.1" ;;
    mdns) license="Unlicense" ;;
    websocket-ws) license="MIT" ;;
    *) license="unknown" ;;
  esac
  echo -e "$name\t$url\t$branch\t$commit\t$license" >>"$LOCK.tmp"
done < <(printf '%s\n' "${REPOS[@]}")

cat "$LOCK.tmp"
mv "$LOCK.tmp" "$LOCK"
echo "== wrote $LOCK"
exit "$status"
