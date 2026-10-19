#!/usr/bin/env bash
# Run a built osk-win binary under wine.
#   ./run.sh osk-win [args...]
#   ./run.sh --release osk-win [args...]
#   ./run.sh oskwin-verify recv
set -euo pipefail
cd "$(dirname "$0")"
source toolchain.sh

PROFILE="debug"
if [ "${1:-}" = "--release" ]; then
  PROFILE="release"
  shift
fi

BIN="${1:?usage: run.sh [--release] <bin-name> [args...]}"
shift
EXE="target/$TARGET/$PROFILE/$BIN.exe"
if [ ! -f "$EXE" ]; then
  echo "no such binary: $EXE (run ./build.sh first)" >&2
  exit 1
fi
exec wine "$EXE" "$@"
