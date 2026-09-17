#!/bin/sh
# Derive assets/app-icon.ico from assets/app-icon.png.
#
# The .ico is committed, so a plain `cargo build` never runs this script; only
# a change to the source PNG does. Scaling uses macOS's own sips, so this is a
# macOS-only step — the cross-compilation container consumes the committed file.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
source=$root/assets/app-icon.png
target=$root/assets/app-icon.ico
stage=$root/target/ico-frames

rm -rf "$stage"
mkdir -p "$stage"
# 256 is both the source size and the format's maximum; anything larger would
# only upscale. Explorer picks the frame matching the current view.
for size in 16 32 48 64 128 256; do
    sips -s format png -Z "$size" "$source" --out "$stage/$size.png" >/dev/null
done

# An .ico is a directory table followed by the frames; PNG frames are legal
# since Vista, so the frames go in untouched and no image library is needed.
python3 - "$stage" "$target" <<'PYTHON'
import struct
import sys
from pathlib import Path

stage, target = Path(sys.argv[1]), Path(sys.argv[2])
sizes = (16, 32, 48, 64, 128, 256)
frames = [(stage / f'{size}.png').read_bytes() for size in sizes]

offset = 6 + 16 * len(sizes)
directory = b''
for size, data in zip(sizes, frames):
    # The table stores width and height in one byte each, so 256 is written as 0.
    directory += struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0,
                             1, 32, len(data), offset)
    offset += len(data)

target.write_bytes(struct.pack('<HHH', 0, 1, len(sizes)) + directory + b''.join(frames))
PYTHON

rm -rf "$stage"
echo "Built $target"
