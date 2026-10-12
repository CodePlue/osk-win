#!/usr/bin/env bash
# Cross-compile the osk-win workspace for Windows.
#   ./build.sh                 # debug build of all crates
#   ./build.sh --release       # release build
#   ./build.sh -p osk-win      # one crate
set -euo pipefail
cd "$(dirname "$0")"
source toolchain.sh
cargo build --target "$TARGET" "$@"
