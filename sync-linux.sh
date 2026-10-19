#!/usr/bin/env bash
# sync-linux.sh: Track, compare, and merge changes from the main Linux OSK repo.
#
# Usage:
#   ./sync-linux.sh log       # View recent commits from Linux OSK
#   ./sync-linux.sh diff      # Compare UI / dictate code with Linux OSK
#   ./sync-linux.sh core      # Sync shared core engine files from Linux OSK
#   ./sync-linux.sh render    # Sync the shared painters (osk/src/render.rs)
#   ./sync-linux.sh check     # Cross-compile and run full test suite
#   ./sync-linux.sh status    # Check sync status and latest commit comparison
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LINUX_REPO="${LINUX_REPO:-$HOME/osk}"

cd "$SCRIPT_DIR"

ensure_remote() {
    if ! git remote get-url linux >/dev/null 2>&1; then
        echo "[sync] Adding git remote 'linux' -> $LINUX_REPO"
        git remote add linux "$LINUX_REPO"
    fi
    if [ -d "$LINUX_REPO/.git" ]; then
        git fetch linux main --quiet 2>/dev/null || true
    fi
}

cmd_status() {
    ensure_remote
    echo "=== OSK Sync Status ==="
    echo "Windows OSK HEAD: $(git rev-parse --short HEAD) - $(git log -1 --pretty=format:%s)"
    if git rev-parse --verify linux/main >/dev/null 2>&1; then
        echo "Linux OSK HEAD:   $(git rev-parse --short linux/main) - $(git log -1 --pretty=format:%s linux/main)"
    else
        echo "Linux OSK HEAD:   [Remote linux/main not fetched]"
    fi
}

cmd_log() {
    ensure_remote
    echo "=== Recent Commits in Linux OSK (linux/main) ==="
    git log -n 15 --oneline --graph --decorate linux/main
}

cmd_diff() {
    ensure_remote
    echo "=== Comparing core engine files ==="
    if [ -d "$LINUX_REPO/osk-core/src" ]; then
        diff -u -r "$LINUX_REPO/osk-core/src" "$SCRIPT_DIR/core/src" || true
    fi
    echo ""
    echo "=== Recent Linux OSK UI changes (last 5 commits touching ui.rs) ==="
    git log -n 5 -p linux/main -- osk/src/ui.rs
}

cmd_core() {
    ensure_remote
    if [ ! -d "$LINUX_REPO/osk-core/src" ]; then
        echo "[sync] Error: Linux repo not found at $LINUX_REPO" >&2
        exit 1
    fi
    echo "[sync] Syncing core engine files from $LINUX_REPO/osk-core/src to $SCRIPT_DIR/core/src..."
    for file in "$LINUX_REPO/osk-core/src"/*.rs; do
        fname="$(basename "$file")"
        if [ "$fname" = "typekey.rs" ]; then
            # Preserve Windows US-QWERTY mapping in typekey.rs
            echo "  - Skipping $fname (Windows-specific SendInput key table preserved)"
            continue
        fi
        echo "  - Updating $fname"
        cp "$file" "$SCRIPT_DIR/core/src/$fname"
    done
    cp "$LINUX_REPO"/osk-core/data/* "$SCRIPT_DIR/core/data/"
    for file in "$LINUX_REPO"/osk-core/tests/*.rs; do
        # The vendored crate is named oskwin_core.
        sed 's/\bosk_core::/oskwin_core::/g' "$file" > "$SCRIPT_DIR/core/tests/$(basename "$file")"
    done
    echo "[sync] Core engine sync complete. Running verification tests..."
    cmd_check
}

cmd_render() {
    ensure_remote
    # render.rs paints through crate::cairo, the GDI implementation of
    # the cairo calls it makes; only its import line differs.
    sed 's/^use cairo::{/use crate::cairo::{self, /' "$LINUX_REPO/osk/src/render.rs" \
        > "$SCRIPT_DIR/osk-win/src/render.rs"
    echo "[sync] render.rs synced. Running verification tests..."
    cmd_check
}

cmd_check() {
    echo "[sync] Compiling workspace and running automated test suites..."
    source "$SCRIPT_DIR/toolchain.sh"
    cargo test --target "$TARGET" --workspace
    echo "[sync] All tests passed cleanly!"
}

CMD="${1:-status}"
case "$CMD" in
    status) cmd_status ;;
    log)    cmd_log ;;
    diff)   cmd_diff ;;
    core)   cmd_core ;;
    render) cmd_render ;;
    check)  cmd_check ;;
    *)
        echo "Usage: $0 {status|log|diff|core|render|check}" >&2
        exit 1
        ;;
esac
