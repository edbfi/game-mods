#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Check a packaged mod's files on Windows, for the unprivileged verify jobs of ci.yml, nightly.yml and release.yml.
#
#   verify-mod-artifact.sh <game>/<mod> <version> <dir>
#
# <dir> holds the files of the mod's zip, extracted. A Rust trainer (<game>/trainer) declares one [[bin]] in its
# Cargo.toml, and its files must hold exactly one .exe, <bin>/<bin>.exe. That executable must carry a manifest and a
# VERSIONINFO whose FileVersion is <version>, and keep the trainer command line: --version prints "<bin> <version>",
# --help exits 0, an unknown option exits 2, and with no game running, check and stop exit 3. No other mod ships an
# executable yet, so one that does fails here until it gets its own check. It runs only the packaged files and
# writes nothing.
set -euo pipefail

die() {
  echo "::error::$*" >&2
  exit 1
}

[[ $# -eq 3 ]] || die "usage: $0 <game>/<mod> <version> <dir>"
dir="$1" version="$2" files="$3"
name='[a-z0-9]+(-[a-z0-9]+)*'
[[ "$dir" =~ ^($name)/($name)$ ]] || die "Mods live in <game>/<mod> kebab-case folders, not $dir"
mod="${BASH_REMATCH[3]}"
number='(0|[1-9][0-9]*)'
[[ "$version" =~ ^$number\.$number\.$number$ ]] || die "Versions look like 1.2.3, not $version"
[[ -d "$files" ]] || die "$files is not a folder"

mapfile -d '' exes < <(find "$files" -type f -iname '*.exe' -print0)
if [[ "$mod" != trainer ]]; then
  [[ ${#exes[@]} -eq 0 ]] || die "$dir ships ${exes[*]}, and only trainers have an executable check"
  echo "$dir ships no executable"
  exit 0
fi

manifest="$(dirname "$0")/../$dir/Cargo.toml"
program="$(python3 -c '
import sys, tomllib
bins = tomllib.load(open(sys.argv[1], "rb")).get("bin", [])
if len(bins) != 1:
    sys.exit(f"::error::{sys.argv[1]} must declare exactly one [[bin]]")
print(bins[0]["name"])
' "$manifest")"
exe="$files/$program/$program.exe"
[[ ${#exes[@]} -eq 1 && -f "$exe" ]] || die "$dir must ship exactly one .exe, $program/$program.exe; found ${exes[*]:-none}"

# The PE's resource types must include VERSIONINFO (16) and MANIFEST (24), and its FileVersion must be the version.
python3 - "$exe" "$version" <<'PY'
import re, struct, sys
path, version = sys.argv[1], sys.argv[2]
data = open(path, "rb").read()
def fail(why):
    sys.exit(f"::error::{path}: {why}")
pe = struct.unpack_from("<I", data, 0x3C)[0]
if data[pe:pe + 4] != b"PE\0\0":
    fail("not a PE image")
sections, optional_size = struct.unpack_from("<H", data, pe + 6)[0], struct.unpack_from("<H", data, pe + 20)[0]
optional = pe + 24
if struct.unpack_from("<H", data, optional)[0] != 0x20B:
    fail("not a PE32+ image")
rva = struct.unpack_from("<I", data, optional + 128)[0]  # data directory 2: resources
offset = None
for i in range(sections):
    header = optional + optional_size + 40 * i
    size, start, raw_size, raw = struct.unpack_from("<IIII", data, header + 8)
    if start <= rva < start + max(size, raw_size):
        offset = raw + rva - start
if not rva or offset is None:
    fail("no resources")
named, ids = struct.unpack_from("<HH", data, offset + 12)
entries = offset + 16 + 8 * named
types = {struct.unpack_from("<I", data, entries + 8 * i)[0] for i in range(ids)}
for kind, label in ((16, "VERSIONINFO"), (24, "MANIFEST")):
    if kind not in types:
        fail(f"no {label} resource")
text = data[offset:].decode("utf-16-le", errors="replace")
if not re.search("FileVersion\0+" + re.escape(version) + "\0", text):
    fail(f"its VERSIONINFO FileVersion is not {version}")
PY

# run <expected status> <args...>: runs the trainer and requires that exit status.
run() {
  local expected="$1" status=0
  shift
  "$exe" "$@" </dev/null >/dev/null || status=$?
  [[ "$status" -eq "$expected" ]] || die "$program $* exited $status, expected $expected"
}

actual="$("$exe" --version </dev/null)"
[[ "$actual" == "$program $version" ]] || die "$program --version printed '$actual', expected '$program $version'"
run 0 --help
run 2 --bogus
# No game runs on the verifying machine, so check and stop find nothing to act on.
run 3 check
run 3 stop
echo "$exe: $actual, with a manifest and VERSIONINFO; --help, usage errors, check and stop behave"
