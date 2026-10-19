#!/usr/bin/env bash
# Linker wrapper for cross-building to Windows.
#
# rustc calls the linker driver without a `--target`, so a bare `clang`
# defaults to the host (ELF) linker and chokes on PE flags like
# `--dynamicbase`. Forcing the Windows target makes clang use lld.
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LM="${LM_DIR:-$HOME/llvm-mingw/llvm-mingw-20261006-ucrt-ubuntu-22.04-x86_64}"

ARGS=()
for arg in "$@"; do
    case "$arg" in
        -lstdc++)     ARGS+=("-l:libc++.a" "-l:libc++abi.a" "-l:libunwind.a") ;;
        -lggml)      ARGS+=("-l:ggml.a") ;;
        -lggml-base) ARGS+=("-l:ggml-base.a") ;;
        -lggml-cpu)  ARGS+=("-l:ggml-cpu.a") ;;
        *)           ARGS+=("$arg") ;;
    esac
done

exec "$LM/bin/clang" --target=x86_64-pc-windows-gnu "${ARGS[@]}"
