#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
set -eu
cd "$(dirname "$0")/.."
ZIG_BIN=${ZIG_BIN:-zig}
if [ "$("$ZIG_BIN" version)" != "0.17.0" ]; then
    echo 'Zig 0.17.0 is required. Put it on PATH or set ZIG_BIN to its executable.' >&2
    exit 1
fi
command -v wsl.exe >/dev/null 2>&1 || {
    echo 'This test requires wsl.exe with Windows executable interoperability.' >&2
    exit 1
}
mkdir -p build
"$ZIG_BIN" c++ -target x86_64-windows-gnu -std=c++17 -O2 -static \
    -Wall -Wextra -Werror -Iinclude src/core.cpp tests/windows_backend_tests.cpp \
    -municode -o build/windows-backend-tests.exe
"$ZIG_BIN" c++ -target x86_64-windows-gnu -std=c++17 -O2 -static \
    -Wall -Wextra -Werror tests/windows_attach_tests.cpp \
    -municode -o build/windows-attach-tests.exe
ZIG_BIN="$ZIG_BIN" ./tools/build-windows.sh
# WSL reaches this shared checkout. The attach test runs the trainer only against copies of the
# dummy executable named metro.exe, in read-only --check and --stop modes.
wsl.exe --cd "$PWD" --exec ./build/windows-backend-tests.exe
wsl.exe --cd "$PWD" --exec ./build/windows-attach-tests.exe
