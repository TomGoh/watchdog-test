#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0
#
# Build the test binary statically for the chosen target arch.
# Usage:   ./scripts/build.sh [aarch64|x86_64]
# Output:  target/<triple>/release/deps/...      the test binaries
#          target/<triple>/wdtest-bins.txt       manifest: absolute paths of
#                                                exactly the binaries this
#                                                build produced (deploy.sh
#                                                pushes only these, never
#                                                stale deps/*-<oldhash>)
#          target/<triple>/wdtest-build.log      cargo's stderr
#
# We use musl for true-static binaries that scp cleanly to any Linux box.

set -euo pipefail

ARCH="${1:-$(uname -m)}"
case "$ARCH" in
    aarch64|arm64)   TRIPLE="aarch64-unknown-linux-musl" ;;
    x86_64|amd64)    TRIPLE="x86_64-unknown-linux-musl"  ;;
    *) echo "Unknown ARCH: $ARCH" >&2; exit 1 ;;
esac

if ! command -v cargo >/dev/null 2>&1 || ! command -v rustup >/dev/null 2>&1; then
    echo "cargo / rustup not found in PATH." >&2
    if [ -n "${SUDO_USER:-}" ]; then
        echo "You are running under sudo — root usually has no Rust toolchain." >&2
        echo "Run the scripts as your normal user; they call sudo themselves where needed." >&2
    else
        echo "Install Rust via https://rustup.rs, then re-run." >&2
    fi
    exit 1
fi

# Verify the target is installed; offer to add it.
if ! rustup target list --installed | grep -qx "$TRIPLE"; then
    echo "Adding rustup target $TRIPLE …"
    rustup target add "$TRIPLE"
fi

cd "$(dirname "$0")/.."
OUT_DIR="target/$TRIPLE"
mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/wdtest-build.log"
JSON="$OUT_DIR/wdtest-build.json"
MANIFEST="$OUT_DIR/wdtest-bins.txt"

echo "Building tests for $TRIPLE …"
cargo test --no-run --release --target "$TRIPLE" --workspace \
    --message-format=json-render-diagnostics \
    >"$JSON" 2> >(tee "$LOG" >&2)

# Every test executable cargo just produced (fresh or up-to-date) is
# reported as a compiler-artifact message with a non-null "executable".
grep -oE '"executable":"[^"]+"' "$JSON" \
    | sed -E 's/^"executable":"(.*)"$/\1/' \
    | sort -u >"$MANIFEST"
rm -f "$JSON"

if [ ! -s "$MANIFEST" ]; then
    echo "Build finished but no test executables were reported; see $LOG" >&2
    exit 1
fi

echo
echo "Test binaries produced (manifest: $MANIFEST):"
sed "s|^$PWD/||" "$MANIFEST"
