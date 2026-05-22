#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0
#
# capture-run.sh — record an autonomous watchdog-test run (dmesg + the
# full deploy.sh stdout) into logs/<YYYY-MM-DD>-<host>-<run-kind>-<HHMM>/.
#
# These are *archival* records, not actively compared by any test —
# their purpose is to preserve "here's what a passing run looked like
# on this hardware on this date" for debugging / onboarding /
# attaching to bug reports.
#
# Usage:
#   ./scripts/capture-run.sh <ssh-target>                  # autonomous run
#   ./scripts/capture-run.sh <ssh-target> --lab <module>   # destructive lab run
#
# Each invocation creates a NEW directory under logs/ — older runs are
# never overwritten.

set -euo pipefail

# ---------------------------------------------------------------------------
# Argument parsing — mirror deploy.sh exactly so capture-run is a thin wrapper.
# ---------------------------------------------------------------------------
LAB_MODULE=""
TARGET=""

while [ $# -gt 0 ]; do
    case "$1" in
        --lab)
            LAB_MODULE="${2:?--lab requires <module>}"
            shift 2
            ;;
        --help|-h)
            sed -n '2,20p' "$0"
            exit 0
            ;;
        -*)
            echo "Unknown option: $1" >&2; exit 2 ;;
        *)
            if [ -z "$TARGET" ]; then TARGET="$1"
            else echo "Unexpected positional arg: $1" >&2; exit 2
            fi
            shift
            ;;
    esac
done

if [ -z "$TARGET" ]; then
    echo "Usage: $0 <TARGET> [--lab <module>]" >&2
    exit 2
fi

cd "$(dirname "$0")/.."

# ---------------------------------------------------------------------------
# Run-dir name encodes the invocation (autonomous vs. lab-<module>).
# ---------------------------------------------------------------------------
DATE=$(date +%Y-%m-%d)
TIME=$(date +%H%M)
HOST_LABEL=$(echo "$TARGET" | tr '/:.' '_')

if [ -n "$LAB_MODULE" ]; then
    SLUG=$(echo "$LAB_MODULE" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/_/g; s/^_+//; s/_+$//')
    RUN_KIND="lab-${SLUG}"
else
    RUN_KIND="autonomous"
fi
RUN_DIR="logs/${DATE}-${HOST_LABEL}-${RUN_KIND}-${TIME}"
mkdir -p "$RUN_DIR"

echo "Recording run into $RUN_DIR/"
echo

# ---------------------------------------------------------------------------
# 1. Pre-run target metadata.
# ---------------------------------------------------------------------------
{
    echo "# meta.txt — written by scripts/capture-run.sh"
    echo "#"
    echo "# Reproduce this run with:"
    if [ -n "$LAB_MODULE" ]; then
        echo "#   ./scripts/capture-run.sh '$TARGET' --lab '$LAB_MODULE'"
    else
        echo "#   ./scripts/capture-run.sh '$TARGET'"
    fi
    echo "#"
    echo "captured-by: $0"
    echo "captured-at: $(date -Iseconds)"
    echo "target:      $TARGET"
    echo "kind:        $RUN_KIND"
    [ -n "$LAB_MODULE" ] && echo "lab-module:  $LAB_MODULE"
    echo
    echo "--- ssh $TARGET 'uname -a; uptime' ---"
    ssh "$TARGET" 'uname -a; uptime' 2>&1 || true
    echo
    echo "--- pre-run lsmod (watchdog-related) ---"
    ssh "$TARGET" 'lsmod | grep -iE "wdt|watchdog|softdog"' 2>&1 || true
    echo
    echo "--- pre-run /sys/class/watchdog inventory ---"
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

# ---------------------------------------------------------------------------
# 2. Pre-run dmesg snapshot.
# ---------------------------------------------------------------------------
DMESG_FILTER='RUST|sbsa-gwdt|sbsa_gwdt|SBSA Generic|watchdog|WDT|wdat|softdog|sp5100|iTCO|hpwdt|it87'

capture_dmesg() {
    local out="$1"
    local raw="${out}.raw"

    # Some targets restrict dmesg to privileged users
    # (kernel.dmesg_restrict=1).  Use non-interactive sudo so capture-run
    # never blocks waiting for a password; sudo failures are preserved in
    # the output file for debugging.
    if ssh "$TARGET" "sudo -n dmesg" > "$raw" 2>&1; then
        grep -iE "$DMESG_FILTER" "$raw" > "$out" || true
    else
        cp "$raw" "$out"
    fi
    rm -f "$raw"
}

capture_dmesg "$RUN_DIR/dmesg-pre.log"
echo "  wrote dmesg-pre.log ($(wc -l <"$RUN_DIR/dmesg-pre.log") lines)"

# ---------------------------------------------------------------------------
# 3. Run deploy.sh, tee everything into tests.log.
# ---------------------------------------------------------------------------
echo
if [ -n "$LAB_MODULE" ]; then
    echo "Running ./scripts/deploy.sh $TARGET --lab $LAB_MODULE …"
    DEPLOY_ARGS=("$TARGET" "--lab" "$LAB_MODULE")
else
    echo "Running ./scripts/deploy.sh $TARGET …"
    DEPLOY_ARGS=("$TARGET")
fi
{
    echo "=========================================="
    echo "Run kind:    $RUN_KIND"
    echo "Started:     $(date -Iseconds)"
    echo "Target:      $TARGET"
    echo "=========================================="
    echo
    ./scripts/deploy.sh "${DEPLOY_ARGS[@]}" 2>&1
    echo
    echo "Finished:    $(date -Iseconds)"
} | tee "$RUN_DIR/tests.log"
echo "  wrote tests.log"

# ---------------------------------------------------------------------------
# 4. Post-run snapshots.
# ---------------------------------------------------------------------------

# Lab mode reboots the target via the watchdog; deploy.sh returns with a
# broken-pipe error and the target is mid-reboot.  Wait for SSH to come back
# before the post-snapshot, otherwise dmesg-post.log just captures SSH errors
# and dmesg-delta.log becomes meaningless.
if [ -n "$LAB_MODULE" ]; then
    echo "Lab mode: waiting for $TARGET to reboot back…"
    deadline=$(( $(date +%s) + 180 ))
    while ! ssh -o ConnectTimeout=3 -o BatchMode=yes "$TARGET" 'true' 2>/dev/null; do
        if [ "$(date +%s)" -gt "$deadline" ]; then
            echo "  WARNING: $TARGET did not come back within 180s; post-snapshot will be incomplete." >&2
            break
        fi
        sleep 5
    done
    if ssh -o ConnectTimeout=3 -o BatchMode=yes "$TARGET" 'true' 2>/dev/null; then
        echo "  $TARGET back online"
        sleep 5   # let udev/journald settle so post dmesg includes early boot
    fi
fi

capture_dmesg "$RUN_DIR/dmesg-post.log"
echo "  wrote dmesg-post.log ($(wc -l <"$RUN_DIR/dmesg-post.log") lines)"

diff "$RUN_DIR/dmesg-pre.log" "$RUN_DIR/dmesg-post.log" \
    | grep '^>' | sed 's/^> //' \
    > "$RUN_DIR/dmesg-delta.log" || true
echo "  wrote dmesg-delta.log ($(wc -l <"$RUN_DIR/dmesg-delta.log") lines)"

{
    echo "--- post-run lsmod (watchdog-related) ---"
    ssh "$TARGET" 'lsmod | grep -iE "wdt|watchdog|softdog"' 2>&1 || true
    echo
    echo "--- post-run /sys/class/watchdog inventory ---"
    ssh "$TARGET" '
        for w in /sys/class/watchdog/watchdog*; do
            [ -e "$w" ] || continue
            echo "$w"
            for f in identity timeout pretimeout state nowayout bootstatus; do
                [ -r "$w/$f" ] && printf "  %-12s = %s\n" "$f" "$(cat "$w/$f" 2>/dev/null)"
            done
        done
    ' 2>&1 || true
} > "$RUN_DIR/meta-post.txt"
echo "  wrote meta-post.txt"

echo
echo "Done — see $RUN_DIR/"
ls -lh "$RUN_DIR/"
