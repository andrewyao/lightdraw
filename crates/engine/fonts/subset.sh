#!/bin/sh
# Rebuilds InterSubset-Regular.ttf from Inter 4 Regular (OFL.txt): Latin-1,
# general punctuation (the ellipsis among it), the euro, trademark, and
# arrows. No layout features, since the engine lays out by advance widths
# alone. Needs fontTools (`pip install fonttools`).
#
#   crates/engine/fonts/subset.sh path/to/Inter-Regular.ttf
set -eu
here=$(dirname "$0")
python3 -m fontTools.subset "$1" \
  --unicodes="U+0020-007E,U+00A0-00FF,U+2010-2027,U+2030-203A,U+20AC,U+2122,U+2190-2193" \
  --layout-features='' \
  --output-file="$here/InterSubset-Regular.ttf"
