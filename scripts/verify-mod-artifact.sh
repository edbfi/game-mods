#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Check a packaged mod's files on Windows, for the unprivileged verify jobs of ci.yml, nightly.yml and release.yml.
#
#   verify-mod-artifact.sh <game>/<mod> <version> <dir>
#
# <dir> holds the files of the mod's zip, extracted. A Rust trainer (<game>/trainer) must hold exactly one .exe, which
# must keep the trainer command line: --version prints "<name> <version>", --help exits 0, an unknown option exits 2,
# and with no game running, check and stop exit 3. No other mod ships an executable yet, so one that does fails
# here until it gets its own check. It runs only the packaged files and writes nothing.
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
[[ ${#exes[@]} -eq 1 ]] || die "$dir must ship exactly one .exe, found ${#exes[@]}"
exe="${exes[0]}"
program="$(basename "$exe")"
program="${program%.*}"

# run <expected status> <args...>: runs the trainer and requires that exit status.
run() {
  local expected="$1" status=0
  shift
  "$exe" "$@" >/dev/null || status=$?
  [[ "$status" -eq "$expected" ]] || die "$program $* exited $status, expected $expected"
}

actual="$("$exe" --version)"
[[ "$actual" == "$program $version" ]] || die "$program --version printed '$actual', expected '$program $version'"
run 0 --help
run 2 --bogus
# No game runs on the verifying machine, so check and stop find nothing to act on.
run 3 check
run 3 stop
echo "$exe: $actual; --help, usage errors, check and stop behave"
