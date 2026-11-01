#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-only
# Starts the REA MCP server from the main checkout's .rea install, so every worktree shares it.
set -eu
root=$(cd "$(git rev-parse --path-format=absolute --git-common-dir)/.." && pwd)
modules=$root/.rea/node_modules
if [ ! -x "$modules/node/bin/node" ]; then
    echo "REA is not installed: run 'just setup-rea' in $root" >&2
    exit 1
fi
if command -v brew >/dev/null 2>&1; then
    prefix=$(brew --prefix)
    if [ -z "${GHIDRA_INSTALL_DIR-}" ] && [ -d "$prefix/opt/ghidra/libexec" ]; then
        export GHIDRA_INSTALL_DIR="$prefix/opt/ghidra/libexec"
    fi
    # The JDK home is libexec on Linux and libexec/openjdk.jdk/Contents/Home on macOS.
    for jdk in "$prefix/opt/openjdk@25/libexec/openjdk.jdk/Contents/Home" "$prefix/opt/openjdk@25/libexec"; do
        if [ -z "${JAVA_HOME-}" ] && [ -x "$jdk/bin/java" ]; then
            export JAVA_HOME="$jdk"
        fi
    done
fi
exec "$modules/node/bin/node" "$modules/rea-agents/scripts/rea.mjs" mcp
