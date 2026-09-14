#!/bin/sh
# Compress a single released executable and drop the bare file, so every
# platform publishes one archive. Info-ZIP records the Unix mode, which keeps
# the executable bit on Linux and macOS after extraction.
set -eu

binary=$1
archive="${binary%.exe}.zip"

rm -f "$archive"
# -j drops the dist/ prefix so the archive contains just the executable.
zip -j -q -9 "$archive" "$binary"
rm -f "$binary"
echo "Built $archive"
