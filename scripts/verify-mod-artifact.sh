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

# The PE must carry a VERSIONINFO (type 16) whose FileVersion is the version, and an application manifest (type 24).
python3 - "$exe" "$version" <<'PY'
import re, struct, sys
path, version = sys.argv[1], sys.argv[2]
data = open(path, "rb").read()
def fail(why):
    sys.exit(f"::error::{path}: {why}")
def u16(at): return struct.unpack_from("<H", data, at)[0]
def u32(at): return struct.unpack_from("<I", data, at)[0]
pe = u32(0x3C)
if data[pe:pe + 4] != b"PE\0\0":
    fail("not a PE image")
optional = pe + 24
if u16(optional) != 0x20B:
    fail("not a PE32+ image")
sections = [struct.unpack_from("<IIII", data, optional + u16(pe + 20) + 40 * i + 8) for i in range(u16(pe + 6))]
def offset(rva):
    for _, start, raw_size, raw in sections:
        if start <= rva < start + raw_size:
            return raw + rva - start
    fail(f"RVA {rva:#x} is in no section")
root = offset(u32(optional + 128))  # data directory 2: resources
def entries(directory):
    named, ids = u16(directory + 12), u16(directory + 14)
    return [struct.unpack_from("<II", data, directory + 16 + 8 * i) for i in range(named + ids)]
def payload(kind, label):
    # Type, then name, then language: take the first resource of the type.
    link = next((link for name, link in entries(root) if name == kind), None)
    for _ in range(2):
        if link is None or not link & 0x80000000:
            fail(f"no {label} resource")
        link = next((link for _, link in entries(root + (link & 0x7FFFFFFF))), None)
    if link is None or link & 0x80000000:
        fail(f"no {label} resource")
    rva, size = struct.unpack_from("<II", data, root + link)
    if not size:
        fail(f"an empty {label} resource")
    return data[offset(rva):offset(rva) + size]
if b"<assembly" not in payload(24, "MANIFEST"):
    fail("a MANIFEST resource that is not a manifest")
info = payload(16, "VERSIONINFO").decode("utf-16-le", errors="replace")
if not re.search("FileVersion\0+" + re.escape(version) + "\0", info):
    fail(f"a VERSIONINFO whose FileVersion is not {version}")
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
