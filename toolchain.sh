#!/usr/bin/env bash
# Source this (or run it as a prefix) to build osk-win for Windows from Linux.
#
#   source toolchain.sh          # then: build.sh, run.sh
#   ./build.sh                   # cross-compile the workspace
#   ./run.sh <args>              # run a built .exe under wine
#
# Toolchain (all per-user, no root):
#   * rustup            -> ~/.rustup (stable + x86_64-pc-windows-gnu std)
#   * llvm-mingw        -> ~/llvm-mingw (Linux-native clang that emits PE)
#   * winlibs mingw     -> ~/mingw   (supplies libgcc.a / libgcc_eh.a)
set -euo pipefail

LM_DIR="${LM_DIR:-$HOME/llvm-mingw/llvm-mingw-20261006-ucrt-ubuntu-22.04-x86_64}"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"

export PATH="$LM_DIR/bin:$CARGO_HOME/bin:$PATH"
export TARGET="${TARGET:-x86_64-pc-windows-gnu}"

# rustc calls the linker without a --target, so use the wrapper that forces
# the Windows PE target (makes clang use lld instead of the host ELF ld).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER="$SCRIPT_DIR/winlink.sh"

# The libgcc aliasing is done once at setup (setup-toolchain.sh), not here.
