#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0
#
# scp the test binaries to the target machine and execute them under
# sudo.  Identity selection happens via the WATCHDOG_TEST_IDENTITY env
# var (see tests_common::pick_watchdog).
#
# Usage:
#   ./scripts/deploy.sh <ssh-target> [arch] [identity] [mode]
#
# Modes:
#   fast      — run only the fast suite (common_conformance + per-driver
#               basic + per-driver _extended).  Non-destructive.  DEFAULT.
#   extended  — same as fast, plus all `--ignored` non-lab tests.  Useful
#               for slow but safe coverage that's marked #[ignore].
#   lab       — run ONLY the lab_dangerous binary, with
#               WATCHDOG_LAB_DANGEROUS=YES_REALLY set; tests in this
#               tier may REBOOT the target machine.
#
# Examples:
#   ./scripts/deploy.sh my-target                                                # arm64, fast tier
#   ./scripts/deploy.sh my-target aarch64 "SBSA Generic Watchdog" extended       # extended tier
#   ./scripts/deploy.sh my-target aarch64 "SBSA Generic Watchdog" lab            # *** WILL REBOOT ***

set -euo pipefail

TARGET="${1:?ssh target required, e.g. my-target}"
ARCH="${2:-aarch64}"
IDENTITY="${3:-}"
MODE="${4:-fast}"

case "$ARCH" in
    aarch64|arm64) TRIPLE="aarch64-unknown-linux-musl" ;;
    x86_64|amd64)  TRIPLE="x86_64-unknown-linux-musl"  ;;
    *) echo "Unknown ARCH: $ARCH" >&2; exit 1 ;;
esac

case "$MODE" in
    fast|extended|lab) ;;
    *) echo "Unknown MODE: $MODE (use fast|extended|lab)" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."

# Build first if no binaries exist
BIN_DIR="target/$TRIPLE/release/deps"
if [ ! -d "$BIN_DIR" ] || [ -z "$(ls -A "$BIN_DIR" 2>/dev/null)" ]; then
    ./scripts/build.sh "$ARCH"
fi

# Pick which binary names we run, per mode.
case "$MODE" in
    fast|extended)
        PATTERN='^(common_conformance|common_extended|sbsa_gwdt|sbsa_gwdt_extended|softdog|sp5100_tco)-'
        ;;
    lab)
        PATTERN='^lab_dangerous-'
        ;;
esac

mapfile -t BINS < <(find "$BIN_DIR" -maxdepth 1 -type f -executable ! -name "*.so" \
                        -printf '%f\n' | grep -E "$PATTERN" | sort)

if [ "${#BINS[@]}" -eq 0 ]; then
    echo "No test binaries match pattern $PATTERN in $BIN_DIR — did the build succeed?" >&2
    exit 1
fi

REMOTE_DIR="/tmp/watchdog-test"
echo "Pushing ${#BINS[@]} test binaries (mode=$MODE) to $TARGET:$REMOTE_DIR …"
ssh "$TARGET" "mkdir -p $REMOTE_DIR && rm -f $REMOTE_DIR/*"
for b in "${BINS[@]}"; do
    scp -q "$BIN_DIR/$b" "$TARGET:$REMOTE_DIR/$b"
done

# Compose the per-tier env + flag combo.
EXTRA_ENV=""
EXTRA_FLAGS="--test-threads=1 --nocapture"
case "$MODE" in
    extended) EXTRA_FLAGS="$EXTRA_FLAGS --include-ignored" ;;
    lab)
        EXTRA_ENV="WATCHDOG_LAB_DANGEROUS=YES_REALLY"
        EXTRA_FLAGS="$EXTRA_FLAGS --include-ignored"
        # Big red warning before the user trips this
        cat <<EOF

================================================================================
  ATTENTION — MODE=lab will run the dangerous tier on $TARGET.
  Tests in this tier may REBOOT the machine when the watchdog fires correctly.
  Press Ctrl-C in the next 5 seconds to abort.
================================================================================

EOF
        sleep 5
        ;;
esac

echo
echo "Running tests on $TARGET (mode=$MODE) …"
echo "============================================================"
# `ssh -t` allocates a pseudo-TTY so sudo's password prompt is visible
# (and an askpass helper / NOPASSWD entry can also work transparently).
for b in "${BINS[@]}"; do
    echo "----- $b -----"
    ssh -t "$TARGET" \
        "${IDENTITY:+WATCHDOG_TEST_IDENTITY='$IDENTITY' }${EXTRA_ENV:+$EXTRA_ENV }sudo -E $REMOTE_DIR/$b $EXTRA_FLAGS" \
        || echo "FAILED: $b (exit $?)"
done
