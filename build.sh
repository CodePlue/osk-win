#!/usr/bin/env bash
# Cross-compile the osk-win workspace for Windows.
#   ./build.sh                 # debug build of all crates
#   ./build.sh --release       # release build
#   ./build.sh -p osk-win      # one crate
set -euo pipefail
cd "$(dirname "$0")"
source toolchain.sh
fix_ggml() {
  for d in target/"$TARGET"/*/build/whisper-rs-sys-*/out; do
    if [ -d "$d" ]; then
      for sub in "lib" "build/ggml/src"; do
        if [ -d "$d/$sub" ]; then
          (cd "$d/$sub" && \
            [ -f ggml.a ] && [ ! -e libggml.a ] && ln -sf ggml.a libggml.a || true; \
            [ -f ggml-base.a ] && [ ! -e libggml-base.a ] && ln -sf ggml-base.a libggml-base.a || true; \
            [ -f ggml-cpu.a ] && [ ! -e libggml-cpu.a ] && ln -sf ggml-cpu.a libggml-cpu.a || true)
        fi
      done
    fi
  done
}

fix_ggml
if ! cargo build --target "$TARGET" "$@"; then
  fix_ggml
  cargo build --target "$TARGET" "$@"
fi
fix_ggml
