#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0
#
# Build the test binary statically for the chosen target arch.
# Usage:   ./scripts/build.sh [aarch64|x86_64]
# Output:  target/<triple>/release/deps/...   and a list of test binaries
#
# We use musl for true-static binaries that scp cleanly to any Linux box;
# falls back to gnu if the musl toolchain isn't installed.

set -euo pipefail

ARCH="${1:-$(uname -m)}"
case "$ARCH" in
    aarch64|arm64)   TRIPLE="aarch64-unknown-linux-musl" ;;
    x86_64|amd64)    TRIPLE="x86_64-unknown-linux-musl"  ;;
    *) echo "Unknown ARCH: $ARCH" >&2; exit 1 ;;
esac

# Verify the target is installed; offer to add it.
if ! rustup target list --installed | grep -qx "$TRIPLE"; then
    echo "Adding rustup target $TRIPLE …"
    rustup target add "$TRIPLE"
fi

cd "$(dirname "$0")/.."
echo "Building tests for $TRIPLE …"
cargo test --no-run --release --target "$TRIPLE" --workspace 2>&1 | tee /tmp/wdtest-build.log

# `cargo test --no-run` prints "Executable …/deps/foo-<hash>" lines.
# Extract them so the user knows what to copy.
echo
echo "Test binaries produced:"
grep -oE 'target/[^[:space:]]+/deps/[^[:space:]]+' /tmp/wdtest-build.log | sort -u
