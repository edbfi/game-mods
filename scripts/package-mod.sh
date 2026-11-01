#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Package a mod and check its zip, for the unprivileged build jobs of release.yml and nightly.yml.
#
#   package-mod.sh <game>/<mod> <version> [<extract-dir>]
#
# Runs the mod's `just package <version>` and requires dist/<game>-<mod>-<version>.zip to be a regular
# file whose entries are regular files or folders that stay inside the folder they are unzipped into.
# With <extract-dir>, also unzips it there, into a new folder, for upload as a nightly artifact.
# Writes zip=<path> and sha256=<hex> to $GITHUB_OUTPUT when it is set.
set -euo pipefail

die() {
  echo "::error::$*" >&2
  exit 1
}

[[ $# -eq 2 || $# -eq 3 ]] || die "usage: $0 <game>/<mod> <version> [<extract-dir>]"
dir="$1" version="$2" extract="${3:-}"
name='[a-z0-9]+(-[a-z0-9]+)*'
[[ "$dir" =~ ^($name)/($name)$ ]] || die "Mods live in <game>/<mod> kebab-case folders, not $dir"
game="${BASH_REMATCH[1]}" mod="${BASH_REMATCH[3]}"
number='(0|[1-9][0-9]*)'
[[ "$version" =~ ^$number\.$number\.$number$ ]] || die "Versions look like 1.2.3, not $version"
cd "$(dirname "$0")/.." # paths are relative to the repo root
[[ -f "$dir/justfile" ]] || die "No mod at $dir"

just --justfile "$dir/justfile" --working-directory "$dir" package "$version"

zip="$dir/dist/$game-$mod-$version.zip"
[[ -f "$zip" && ! -L "$zip" ]] || die "$zip was not built"
python3 - "$zip" <<'PY'
import stat, sys, zipfile
infos = zipfile.ZipFile(sys.argv[1]).infolist()
if not any(not info.is_dir() for info in infos):
    sys.exit("::error::The zip holds no files")
seen = set()
for info in infos:
    name = info.filename
    path = name.removesuffix("/") if info.is_dir() else name
    parts = path.split("/")
    if "\\" in name or ":" in parts[0] or any(part in ("", ".", "..") for part in parts):
        sys.exit(f"::error::Unsafe zip entry {name!r}")
    # Windows and Vortex unzip case-insensitively, so case-only twins overwrite each other.
    if path.lower() in seen:
        sys.exit(f"::error::Zip entry {name!r} is a duplicate")
    seen.add(path.lower())
    kind = stat.S_IFMT(info.external_attr >> 16)
    if kind not in (0, stat.S_IFDIR if info.is_dir() else stat.S_IFREG):
        sys.exit(f"::error::Zip entry {name!r} is not a regular file or folder")
print(f"{len(infos)} entries")
PY
sha="$(sha256sum "$zip" | cut -d' ' -f1)"
echo "$zip: $(stat -c %s "$zip") bytes, sha256 $sha"

if [[ -n "$extract" ]]; then
  [[ ! -e "$extract" ]] || die "$extract already exists"
  mkdir -p "$extract"
  python3 -m zipfile -e "$zip" "$extract"
  # The artifact must hold exactly the zip's files: no links, nothing lost.
  [[ -z "$(find "$extract" ! -type f ! -type d -print -quit)" ]] || die "$extract holds something other than files"
  files="$(find "$extract" -type f | wc -l)"
  entries="$(python3 -c 'import sys, zipfile; print(sum(not i.is_dir() for i in zipfile.ZipFile(sys.argv[1]).infolist()))' "$zip")"
  [[ "$files" -eq "$entries" ]] || die "$extract holds $files files; the zip has $entries"
  echo "Extracted $files files to $extract"
fi

if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
  { echo "zip=$zip"; echo "sha256=$sha"; } >>"$GITHUB_OUTPUT"
fi
