# SPDX-License-Identifier: AGPL-3.0-only
# Every <game>/<mod>/justfile is a mod; recipes here run it from its own folder.
set shell := ["bash", "-euo", "pipefail", "-c"]

# List available commands.
default:
    @just --list

# Run every mod's tests.
test: (each "test")

# Build every mod.
build: (each "build")

# Run what CI runs: prek, then every mod's tests and build.
ci:
    prek run --all-files --hook-stage manual
    just test
    just build

[private]
each recipe:
    shopt -s nullglob; for file in */*/justfile; do \
        dir=${file%/justfile}; echo "==> $dir: {{recipe}}"; \
        just --justfile "$file" --working-directory "$dir" {{recipe}}; \
    done

# Install the pinned REA (needs npm; Ghidra and OpenJDK 25 from Homebrew are optional), then check it.
setup-rea:
    npm ci --prefix .rea --no-audit --no-fund
    .rea/node_modules/node/bin/node .rea/node_modules/rea-agents/scripts/rea.mjs doctor
