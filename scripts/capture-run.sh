#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0
#
# capture-run.sh — record a watchdog-test run (dmesg + each test
# binary's stdout) into logs/<YYYY-MM-DD>-<host>-<driver-slug>/.
#
# These are *archival* records, not actively compared by any test —
# their purpose is to preserve "here's what a passing run looked like
# on this hardware on this date" for debugging / onboarding /
# attaching to bug reports.
#
# Usage:
#   ./scripts/capture-run.sh <ssh-target> [arch] [identity] [mode]
#
# Examples:
#   ./scripts/capture-run.sh my-target
#   ./scripts/capture-run.sh my-target aarch64 "SBSA Generic Watchdog"
#   ./scripts/capture-run.sh my-target aarch64 "SBSA Generic Watchdog" extended
#
# Each invocation creates a NEW directory under logs/ — older runs are
# never overwritten.

set -euo pipefail

TARGET="${1:?ssh target required, e.g. my-target}"
ARCH="${2:-aarch64}"
IDENTITY="${3:-SBSA Generic Watchdog}"
MODE="${4:-fast}"

cd "$(dirname "$0")/.."

# Slugify the driver identity into a safe directory name component.
# "SBSA Generic Watchdog" -> "sbsa_generic_watchdog"
SLUG=$(echo "$IDENTITY" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -E 's/[^a-z0-9]+/_/g; s/^_+//; s/_+$//')

DATE=$(date +%Y-%m-%d)
TIME=$(date +%H%M)
HOST_LABEL=$(echo "$TARGET" | tr '/:.' '_')
RUN_DIR="logs/${DATE}-${HOST_LABEL}-${SLUG}-${MODE}-${TIME}"
mkdir -p "$RUN_DIR"

echo "Recording run into $RUN_DIR/"
echo

# 1. Pre-run target metadata.
{
    echo "# meta.txt — written by scripts/capture-run.sh"
    echo "#"
    echo "# Run this exact line on a build host to reproduce:"
    echo "#   ./scripts/capture-run.sh '$TARGET' '$ARCH' '$IDENTITY' '$MODE'"
    echo "#"
    echo "captured-by: $0"
    echo "captured-at: $(date -Iseconds)"
    echo "target:      $TARGET   # SSH destination passed to the deploy script"
    echo "arch:        $ARCH     # build / target arch (aarch64 | x86_64)"
    echo "identity:    $IDENTITY # WATCHDOG_TEST_IDENTITY env var passed to test binaries"
    echo "mode:        $MODE     # tier: fast | extended | lab"
    echo
    echo "--- ssh $TARGET 'uname -a; uptime' ---"
    ssh "$TARGET" 'uname -a; uptime' 2>&1 || true
    echo
    echo "--- ssh $TARGET 'lsmod | grep -iE \"wdt|watchdog\"' ---"
    ssh "$TARGET" 'lsmod | grep -iE "wdt|watchdog"' 2>&1 || true
    echo
    echo "--- ssh $TARGET '/sys/class/watchdog inventory' ---"
    ssh "$TARGET" '
        for w in /sys/class/watchdog/watchdog*; do
            [ -e "$w" ] || continue
            echo "$w"
            for f in identity timeout pretimeout state nowayout bootstatus; do
                [ -r "$w/$f" ] && printf "  %-12s = %s\n" "$f" "$(cat "$w/$f" 2>/dev/null)"
            done
        done
    ' 2>&1 || true
} > "$RUN_DIR/meta.txt"
echo "  wrote meta.txt"

# 2. Pre-run dmesg snapshot (full, sbsa/wdt-relevant lines).
ssh "$TARGET" 'dmesg | grep -iE "RUST|sbsa-gwdt|sbsa_gwdt|SBSA Generic|watchdog|WDT|wdat|softdog|sp5100"' \
    > "$RUN_DIR/dmesg-pre.log" 2>&1 || true
echo "  wrote dmesg-pre.log ($(wc -l <"$RUN_DIR/dmesg-pre.log") lines)"

# 3. Run the test deploy and tee to a per-mode tests log.
echo
echo "Running ./scripts/deploy.sh $TARGET $ARCH '$IDENTITY' $MODE …"
{
    echo "=========================================="
    echo "Tier:       $MODE"
    echo "Started:    $(date -Iseconds)"
    echo "Target:     $TARGET"
    echo "=========================================="
    echo
    ./scripts/deploy.sh "$TARGET" "$ARCH" "$IDENTITY" "$MODE" 2>&1
    echo
    echo "Finished:   $(date -Iseconds)"
} | tee "$RUN_DIR/tests-${MODE}.log"
echo "  wrote tests-${MODE}.log"

# 4. Post-run dmesg snapshot — captures everything the tests caused
#    the kernel to emit during the run.
ssh "$TARGET" 'dmesg | grep -iE "RUST|sbsa-gwdt|sbsa_gwdt|SBSA Generic|watchdog|WDT|wdat|softdog|sp5100"' \
    > "$RUN_DIR/dmesg-post.log" 2>&1 || true
echo "  wrote dmesg-post.log ($(wc -l <"$RUN_DIR/dmesg-post.log") lines)"

# 5. The diff between pre and post is the "what this run produced"
#    summary — usually the most interesting view.
diff "$RUN_DIR/dmesg-pre.log" "$RUN_DIR/dmesg-post.log" \
    | grep '^>' | sed 's/^> //' \
    > "$RUN_DIR/dmesg-delta.log" || true
echo "  wrote dmesg-delta.log ($(wc -l <"$RUN_DIR/dmesg-delta.log") lines)"

echo
echo "Done — see $RUN_DIR/"
ls -lh "$RUN_DIR/"
