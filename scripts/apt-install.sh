#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Install Ubuntu packages on a CI runner: apt-install.sh <package>...
# A stalled mirror once held apt for a whole job, so each download has a deadline and is retried. Installing runs
# offline afterwards, under a longer deadline of its own.
set -euo pipefail

(($#)) || { echo "usage: $0 <package>..." >&2; exit 2; }
opts=(-q -o Acquire::Retries=3 -o Acquire::http::Timeout=30 -o Acquire::https::Timeout=30 -o DPkg::Lock::Timeout=120)
for try in 1 2 3; do
  if sudo timeout -k 30 300 apt-get "${opts[@]}" update &&
    sudo timeout -k 30 300 apt-get "${opts[@]}" install -y --download-only "$@"; then
    exec sudo timeout -k 30 600 apt-get "${opts[@]}" install -y --no-download "$@"
  fi
  echo "::warning::apt could not fetch $* (try $try of 3)"
  ((try == 3)) || sleep 15
done
echo "::error::apt could not fetch $* in 3 tries" >&2
exit 1
