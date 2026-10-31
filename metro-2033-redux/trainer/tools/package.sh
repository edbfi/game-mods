#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Usage: tools/package.sh <version>. Run after tools/build-windows.sh.
set -eu
cd "$(dirname "$0")/.."
version=${1:?usage: tools/package.sh <version>}
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "Bad version: $version" >&2; exit 1; }
test -f build/metro-trainer.exe || { echo 'Run tools/build-windows.sh first.' >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo 'Python 3 is required for its zipfile command.' >&2; exit 1; }
stage=dist/stage/metro-trainer
zip=dist/metro-2033-redux-trainer-$version.zip
rm -rf dist/stage "$zip"
mkdir -p "$stage"
cp build/metro-trainer.exe config/metro-trainer.ini README.md "$stage/"
cp ../../LICENSE "$stage/"
python3 -m zipfile -c "$zip" "$stage"
rm -rf dist/stage
printf 'Created %s (all features disabled in the example INI).\n' "$zip"
