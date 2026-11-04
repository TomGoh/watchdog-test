#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0
#
# capture-run.sh — record a watchdog-test run (dmesg + the full deploy.sh
# stdout) into logs/<YYYY-MM-DD>-<host>-<run-kind>-<HHMM>/.
#
# These are *archival* records, not actively compared by any test —
# their purpose is to preserve "here's what a passing run looked like
# on this hardware on this date" for debugging / onboarding /
# attaching to bug reports.
#
# Usage:
#   ./scripts/capture-run.sh                              # THIS machine, autonomous
#   ./scripts/capture-run.sh --lab <module>               # THIS machine, lab (REBOOTS it)
#   ./scripts/capture-run.sh --finish logs/<run-dir>      # after a local lab reboot:
#                                                         #   collect the post-run snapshots
#   ./scripts/capture-run.sh <ssh-target>                 # remote over SSH, autonomous
#   ./scripts/capture-run.sh <ssh-target> --lab <module>  # remote over SSH, lab
#
# Run as your normal user (not with sudo); root is obtained via sudo
# where needed.  Each invocation creates a NEW directory under logs/ —
# older runs are never overwritten.

set -euo pipefail

# ---------------------------------------------------------------------------
# Argument parsing — mirror deploy.sh so capture-run is a thin wrapper.
# ---------------------------------------------------------------------------
LAB_MODULE=""
TARGET=""
FINISH_DIR=""

while [ $# -gt 0 ]; do
    case "$1" in
        --lab)
            LAB_MODULE="${2:?--lab requires <module>}"
            shift 2
            ;;
        --finish)
            FINISH_DIR="${2:?--finish requires logs/<run-dir>}"
            shift 2
            ;;
        --help|-h)
            sed -n '2,22p' "$0"
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

if [ -n "$FINISH_DIR" ] && { [ -n "$TARGET" ] || [ -n "$LAB_MODULE" ]; }; then
    echo "--finish takes only the run directory (local mode)." >&2
    exit 2
fi

cd "$(dirname "$0")/.."

# shellcheck source=lib/target.sh
. scripts/lib/target.sh

# ---------------------------------------------------------------------------
# Shared snapshot helpers.
# ---------------------------------------------------------------------------
DMESG_FILTER='RUST|sbsa-gwdt|sbsa_gwdt|SBSA Generic|watchdog|WDT|wdat|softdog|sp5100|iTCO|hpwdt|it87'

# Some targets restrict dmesg to privileged users (kernel.dmesg_restrict=1).
# Use non-interactive sudo so capture-run never blocks waiting for a
# password; sudo failures are preserved in the output file for debugging.
capture_dmesg() {
    local out="$1"
    local raw="${out}.raw"
    if target_sh "$SUDO_N dmesg" > "$raw" 2>&1; then
        grep -iE "$DMESG_FILTER" "$raw" > "$out" || true
    else
        cp "$raw" "$out"
    fi
    rm -f "$raw"
}

write_watchdog_inventory() {
    target_sh '
        for w in /sys/class/watchdog/watchdog*; do
            [ -e "$w" ] || continue
            echo "$w"
            for f in identity timeout pretimeout state nowayout bootstatus; do
                [ -r "$w/$f" ] && printf "  %-12s = %s\n" "$f" "$(cat "$w/$f" 2>/dev/null)"
            done
        done
    ' 2>&1 || true
}

capture_post() {
    local run_dir="$1"

    capture_dmesg "$run_dir/dmesg-post.log"
    echo "  wrote dmesg-post.log ($(wc -l <"$run_dir/dmesg-post.log") lines)"

    diff "$run_dir/dmesg-pre.log" "$run_dir/dmesg-post.log" \
        | grep '^>' | sed 's/^> //' \
        > "$run_dir/dmesg-delta.log" || true
    echo "  wrote dmesg-delta.log ($(wc -l <"$run_dir/dmesg-delta.log") lines)"

    {
        echo "--- post-run uname -a; uptime ---"
        target_sh 'uname -a; uptime' 2>&1 || true
        echo
        echo "--- post-run lsmod (watchdog-related) ---"
        target_sh 'lsmod | grep -iE "wdt|watchdog|softdog"' 2>&1 || true
        echo
        echo "--- post-run /sys/class/watchdog inventory ---"
        write_watchdog_inventory
    } > "$run_dir/meta-post.txt"
    echo "  wrote meta-post.txt"
}

# ---------------------------------------------------------------------------
# --finish: complete a LOCAL lab run after the watchdog rebooted this box.
# ---------------------------------------------------------------------------
if [ -n "$FINISH_DIR" ]; then
    FINISH_DIR="${FINISH_DIR%/}"
    if [ ! -f "$FINISH_DIR/meta.txt" ] || [ ! -f "$FINISH_DIR/dmesg-pre.log" ]; then
        echo "$FINISH_DIR does not look like a capture-run directory (no meta.txt / dmesg-pre.log)." >&2
        exit 1
    fi
    target_ensure_sudo
    echo "Finishing $FINISH_DIR/ on $TARGET_DESC"

    {
        echo
        echo "=========================================="
        echo "Resumed by --finish: $(date -Iseconds)"
        echo "Current boot uptime: $(uptime)"
        echo "(If the machine rebooted, the lines above were the last ones"
        echo " flushed to disk before the watchdog reset.)"
        echo "=========================================="
    } >> "$FINISH_DIR/tests.log"

    capture_post "$FINISH_DIR"

    # The interesting kernel lines (the watchdog arming / firing) belong to
    # the PREVIOUS boot; current dmesg can't see them.  Pull them from the
    # journal when it's persistent.
    if command -v journalctl >/dev/null 2>&1 \
        && $SUDO_N journalctl -k -b -1 --no-pager > "$FINISH_DIR/dmesg-prevboot.log.raw" 2>&1; then
        grep -iE "$DMESG_FILTER" "$FINISH_DIR/dmesg-prevboot.log.raw" \
            > "$FINISH_DIR/dmesg-prevboot.log" || true
        echo "  wrote dmesg-prevboot.log ($(wc -l <"$FINISH_DIR/dmesg-prevboot.log") lines, from journalctl -k -b -1)"
    else
        echo "  previous-boot kernel log unavailable (journal not persistent?) — skipped dmesg-prevboot.log"
    fi
    rm -f "$FINISH_DIR/dmesg-prevboot.log.raw"
    rm -f "$FINISH_DIR/PENDING-FINISH"

    echo
    echo "Done — see $FINISH_DIR/"
    ls -lh "$FINISH_DIR/"
    exit 0
fi

target_ensure_sudo

# ---------------------------------------------------------------------------
# Run-dir name encodes the invocation (autonomous vs. lab-<module>).
# ---------------------------------------------------------------------------
DATE=$(date +%Y-%m-%d)
TIME=$(date +%H%M)
HOST_LABEL=$(echo "$TARGET_LABEL" | tr '/:.' '_')

if [ -n "$LAB_MODULE" ]; then
    SLUG=$(echo "$LAB_MODULE" | tr '[:upper:]' '[:lower:]' | sed -E 's/[^a-z0-9]+/_/g; s/^_+//; s/_+$//')
    RUN_KIND="lab-${SLUG}"
else
    RUN_KIND="autonomous"
fi
RUN_DIR="logs/${DATE}-${HOST_LABEL}-${RUN_KIND}-${TIME}"
mkdir -p "$RUN_DIR"

echo "Recording run on $TARGET_DESC into $RUN_DIR/"
echo

# Reproduce line, e.g. "./scripts/capture-run.sh --lab 'softdog-rust'".
REPRO="./scripts/capture-run.sh"
[ -n "$TARGET" ]     && REPRO="$REPRO '$TARGET'"
[ -n "$LAB_MODULE" ] && REPRO="$REPRO --lab '$LAB_MODULE'"

# ---------------------------------------------------------------------------
# 1. Pre-run target metadata.
# ---------------------------------------------------------------------------
{
    echo "# meta.txt — written by scripts/capture-run.sh"
    echo "#"
    echo "# Reproduce this run with:"
    echo "#   $REPRO"
    echo "#"
    echo "captured-by: $0"
    echo "captured-at: $(date -Iseconds)"
    echo "mode:        $TARGET_MODE"
    echo "target:      ${TARGET:-<local> $TARGET_LABEL}"
    echo "kind:        $RUN_KIND"
    [ -n "$LAB_MODULE" ] && echo "lab-module:  $LAB_MODULE"
    echo
    echo "--- uname -a; uptime ---"
    target_sh 'uname -a; uptime' 2>&1 || true
    echo
    echo "--- pre-run lsmod (watchdog-related) ---"
    target_sh 'lsmod | grep -iE "wdt|watchdog|softdog"' 2>&1 || true
    echo
    echo "--- pre-run /sys/class/watchdog inventory ---"
    write_watchdog_inventory
} > "$RUN_DIR/meta.txt"
echo "  wrote meta.txt"

# ---------------------------------------------------------------------------
# 2. Pre-run dmesg snapshot.
# ---------------------------------------------------------------------------
capture_dmesg "$RUN_DIR/dmesg-pre.log"
echo "  wrote dmesg-pre.log ($(wc -l <"$RUN_DIR/dmesg-pre.log") lines)"

# ---------------------------------------------------------------------------
# Local lab: this very machine is about to be reset by the watchdog, which
# kills this script and drops anything still in the page cache.  Keep the
# log flushed, and leave instructions for collecting the post-run half.
# ---------------------------------------------------------------------------
SYNC_PID=""
if is_local && [ -n "$LAB_MODULE" ]; then
    FINISH_CMD="./scripts/capture-run.sh --finish $RUN_DIR"
    echo "After the reboot, run: $FINISH_CMD" > "$RUN_DIR/PENDING-FINISH"
    cat <<EOF

  LOCAL LAB RUN: if the watchdog works, THIS machine resets mid-run and
  this script dies with it.  After it boots back up, log in, cd into this
  repo and run:

      $FINISH_CMD

  to record dmesg-post / dmesg-delta / meta-post (+ the previous boot's
  kernel log, if the journal is persistent).

EOF
    sync
    ( while sleep 1; do sync; done ) &
    SYNC_PID=$!
    trap '[ -n "$SYNC_PID" ] && kill "$SYNC_PID" 2>/dev/null || true' EXIT
fi

# ---------------------------------------------------------------------------
# 3. Run deploy.sh, tee everything into tests.log.
# ---------------------------------------------------------------------------
DEPLOY_ARGS=()
[ -n "$TARGET" ]     && DEPLOY_ARGS+=("$TARGET")
[ -n "$LAB_MODULE" ] && DEPLOY_ARGS+=("--lab" "$LAB_MODULE")

echo
echo "Running ./scripts/deploy.sh ${DEPLOY_ARGS[*]:-} …"
{
    echo "=========================================="
    echo "Run kind:    $RUN_KIND"
    echo "Started:     $(date -Iseconds)"
    echo "Target:      $TARGET_DESC"
    echo "=========================================="
    echo
    # Don't let a failing deploy skip the post-run snapshots below.
    rc=0
    ./scripts/deploy.sh ${DEPLOY_ARGS[@]+"${DEPLOY_ARGS[@]}"} 2>&1 || rc=$?
    [ "$rc" -ne 0 ] && echo "deploy.sh exited with status $rc"
    echo
    echo "Finished:    $(date -Iseconds)"
} | tee "$RUN_DIR/tests.log"
echo "  wrote tests.log"

# ---------------------------------------------------------------------------
# 4. Post-run snapshots.
# ---------------------------------------------------------------------------

# Remote lab mode reboots the target via the watchdog; deploy.sh returns
# with a broken-pipe error and the target is mid-reboot.  Wait for SSH to
# come back before the post-snapshot, otherwise dmesg-post.log just captures
# SSH errors and dmesg-delta.log becomes meaningless.
# (Local lab: if we got here, the machine did NOT reboot — the lab check
# failed — so snapshot right away.)
if [ -n "$LAB_MODULE" ] && ! is_local; then
    echo "Lab mode: waiting for $TARGET to reboot back…"
    deadline=$(( $(date +%s) + 180 ))
    while ! target_reachable; do
        if [ "$(date +%s)" -gt "$deadline" ]; then
            echo "  WARNING: $TARGET did not come back within 180s; post-snapshot will be incomplete." >&2
            break
        fi
        sleep 5
    done
    if target_reachable; then
        echo "  $TARGET back online"
        sleep 5   # let udev/journald settle so post dmesg includes early boot
    fi
fi
rm -f "$RUN_DIR/PENDING-FINISH"

capture_post "$RUN_DIR"

echo
echo "Done — see $RUN_DIR/"
ls -lh "$RUN_DIR/"
