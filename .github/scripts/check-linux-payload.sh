#!/bin/bash
#
# check-linux-payload.sh — assert an INSTALLED tmp-companion .deb/.rpm actually
# put its files where a Linux desktop looks for them.
#
# Run by ci.yml's bundle-linux job after installing the package it just built.
# A `tauri build` proves the bundler config parses; only an install proves the
# udev rule, the desktop entry and the hicolor icon tree landed correctly — and
# a wrong hicolor DIRECTORY is invisible to every other check, because the file
# is present and correct, just filed where no icon theme will look for it.
# PAYLOAD_ROOT lets this run against an UNPACKED package tree instead of the
# real filesystem (`PAYLOAD_ROOT=.../data bash check-linux-payload.sh`), so the
# check itself can be exercised locally without installing anything. CI leaves
# it unset and checks the installed system.
set -euo pipefail
ROOT="${PAYLOAD_ROOT:-}"

fail=0
need() { # <path> <what it is>
  if [ -e "$1" ]; then
    echo "  ok    $2: $1"
  else
    echo "  MISS  $2: $1" >&2
    fail=1
  fi
}

echo "binary + resources"
need "$ROOT/usr/bin/tmp-companion" "main binary"

echo "device access"
# Without this the app cannot open the unit at all: /dev/hidraw* is root-only by
# default, so a missing rule is an EACCES at connect time, not a cosmetic slip.
need "$ROOT/usr/lib/udev/rules.d/70-fender-tone-master-pro.rules" "udev rule"

echo "desktop entry"
desktop=$(find "$ROOT/usr/share/applications" -name '*.desktop' -exec grep -l '^Exec=tmp-companion' {} + 2>/dev/null | head -1 || true)
if [ -n "$desktop" ]; then
  echo "  ok    desktop entry: $desktop"
  # StartupWMClass is what lets the shell associate the running window with this
  # launcher; without it GNOME shows a second, generic icon for the live window.
  grep -q '^StartupWMClass=' "$desktop" || { echo "  MISS  StartupWMClass in $desktop" >&2; fail=1; }
else
  echo "  MISS  desktop entry with Exec=tmp-companion" >&2
  fail=1
fi

# PNG IHDR: width is a big-endian u32 at byte 16, height at byte 20. Read with
# od rather than a Python/ImageMagick dependency — the minimal Fedora image in
# the rpm leg ships neither, and od is in every base image.
#
# Bytes are read one at a time (`-tu1`) and recombined in awk rather than with
# GNU's `-tu4 --endian=big`: that flag does not exist in the BSD od macOS ships,
# and PAYLOAD_ROOT is meant to make this runnable on a maintainer's own machine.
be32() { # <file> <byte offset> -> unsigned 32-bit big-endian value at that offset
  od -An -tu1 -j"$2" -N4 "$1" 2>/dev/null |
    awk '{ print $1 * 16777216 + $2 * 65536 + $3 * 256 + $4; exit }'
}

png_dim() { # <file> -> "WxH"
  printf '%sx%s' "$(be32 "$1" 16)" "$(be32 "$1" 20)"
}

echo "hicolor icons"
# Every size we ship must land in a directory hicolor actually defines, AND the
# file's real pixel size must match what that directory declares. Checking only
# the path would miss the entire bug class: `256x256@2` means "256 logical px at
# scale 2" = 512 PHYSICAL px, so a 256px file filed there is present, readable,
# and wrong. Compare the IHDR against the directory name rather than trusting it.
for size in 32x32 64x64 128x128 256x256; do
  icon="$ROOT/usr/share/icons/hicolor/$size/apps/tmp-companion.png"
  if [ ! -e "$icon" ]; then
    echo "  MISS  $size icon: $icon" >&2
    fail=1
    continue
  fi
  got=$(png_dim "$icon")
  if [ "$got" = "$size" ]; then
    echo "  ok    $size icon: $got px"
  else
    echo "  MISS  $size icon: directory says $size but the file is $got px" >&2
    fail=1
  fi
done
# Glob rather than `find -printf`: that is GNU-only too, and this sweep is the
# half that catches an icon filed somewhere nothing looks (the `256x256@2` case).
for icon in "$ROOT"/usr/share/icons/hicolor/*/apps/tmp-companion.png; do
  [ -e "$icon" ] || continue
  dir=$(basename "$(dirname "$(dirname "$icon")")")
  case "$dir" in
    32x32 | 64x64 | 128x128 | 256x256) ;;
    *)
      echo "  MISS  icon filed in unexpected hicolor dir: $dir" >&2
      fail=1
      ;;
  esac
done

if [ "$fail" -ne 0 ]; then
  echo "payload check FAILED" >&2
  exit 1
fi
echo "payload check passed"
