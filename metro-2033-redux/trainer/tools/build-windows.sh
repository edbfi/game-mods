#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu
cd "$(dirname "$0")/.."
# Pinned: the trainer's hooks were built and validated with Zig 0.17.0, and CI installs the same version.
ZIG_BIN=${ZIG_BIN:-zig}
if [ "$("$ZIG_BIN" version)" != "0.17.0" ]; then
    echo 'Zig 0.17.0 is required. Put it on PATH or set ZIG_BIN to its executable.' >&2
    exit 1
fi
mkdir -p build
"$ZIG_BIN" c++ -target x86_64-windows-gnu -std=c++17 -O2 -static \
    -Wall -Wextra -Werror -Iinclude src/core.cpp src/presentation.cpp src/windows_console.cpp src/windows_main.cpp \
    -lbcrypt -lwinmm -municode -o build/metro-trainer.exe
# A build check only: the file is a PE image. It says nothing about runtime behaviour.
[ "$(head -c 2 build/metro-trainer.exe)" = MZ ] || { echo 'build/metro-trainer.exe is not a PE image.' >&2; exit 1; }
