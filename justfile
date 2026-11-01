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

[private]
each recipe:
    shopt -s nullglob; for file in */*/justfile; do \
        dir=${file%/justfile}; echo "==> $dir: {{recipe}}"; \
        just --justfile "$file" --working-directory "$dir" {{recipe}}; \
    done
