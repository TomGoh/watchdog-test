#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-2.0
#
# Autonomous deploy: SSH to <TARGET>, autodetect arch, autodetect every
# loadable watchdog driver in the kernel tree, run the appropriate test
# set against each discovered identity.
#
# Usage:
#   ./scripts/deploy.sh <TARGET>                  # autonomous, reboot-safe
#   ./scripts/deploy.sh <TARGET> --lab <module>   # lab validation, single named module
#
# The autonomous path:
#   1. Resolve target arch via `ssh $TARGET uname -m`.
#   2. Build for that arch if no binaries are cached.
#   3. Snapshot pre-existing watchdogs (so we know what we loaded vs.
#      what was already there).
#   4. Bulk-modprobe every module under
#      /lib/modules/$(uname -r)/kernel/drivers/watchdog/ — failures are
#      silently ignored (wrong-hardware drivers fail with -ENODEV).
#   5. Re-enumerate /sys/class/watchdog/* — that's the test list.
#   6. Push test binaries.
#   7. For each discovered identity:
#        - Known driver  → run common_conformance + common_extended +
#                           the per-driver binary.
#        - Unknown driver → run common_conformance + common_extended only
#                           (basic conformance).
#      All gated by WATCHDOG_TEST_IDENTITY=<id> and --include-ignored.
#   8. rmmod every module we loaded (best-effort; pre-existing untouched).
#
# Reboot safety: every test goes through with_open(...) which always
# magic-V closes.  The non-lab path never fires the watchdog.  Future
# test additions that drop a Watchdog without a magic-V close MUST be
# placed in lab_dangerous.rs, not the per-driver file.

set -euo pipefail

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
LAB_MODULE=""
TARGET=""

while [ $# -gt 0 ]; do
    case "$1" in
        --lab)
            LAB_MODULE="${2:?--lab requires <module> (e.g. softdog-drv)}"
            shift 2
            ;;
        --help|-h)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        -*)
            echo "Unknown option: $1" >&2
            exit 2
            ;;
        *)
            if [ -z "$TARGET" ]; then
                TARGET="$1"
            else
                echo "Unexpected positional arg: $1" >&2
                exit 2
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
# Per-driver tests table.  Adding a new Rust-port driver = one new
# crates/tests/tests/<driver>.rs + one case arm here.
# ---------------------------------------------------------------------------
per_driver_binary_for_identity() {
    case "$1" in
        "SBSA Generic Watchdog")    echo "sbsa_gwdt" ;;
        "SP5100 TCO Watchdog"|"SP5100 TCO timer") echo "sp5100_tco" ;;
        "Software Watchdog (Rust)") echo "softdog" ;;
        *) return 1 ;;
    esac
}

# ---------------------------------------------------------------------------
# Resolve arch from target, build if needed.
# ---------------------------------------------------------------------------
echo "Resolving target arch via SSH …"
REMOTE_UNAME="$(ssh "$TARGET" 'uname -m')"
case "$REMOTE_UNAME" in
    aarch64|arm64) ARCH="aarch64"; TRIPLE="aarch64-unknown-linux-musl" ;;
    x86_64|amd64)  ARCH="x86_64";  TRIPLE="x86_64-unknown-linux-musl"  ;;
    *) echo "Unsupported target arch: $REMOTE_UNAME" >&2; exit 1 ;;
esac
echo "  target arch = $REMOTE_UNAME → $TRIPLE"

BIN_DIR="target/$TRIPLE/release/deps"
if [ ! -d "$BIN_DIR" ] || [ -z "$(ls -A "$BIN_DIR" 2>/dev/null)" ]; then
    ./scripts/build.sh "$ARCH"
fi

# ---------------------------------------------------------------------------
# Helpers shared by both modes.
# ---------------------------------------------------------------------------
REMOTE_DIR="/tmp/watchdog-test"

push_binaries() {
    local pattern="$1"
    mapfile -t BINS < <(find "$BIN_DIR" -maxdepth 1 -type f -executable ! -name "*.so" \
                            -printf '%f\n' | grep -E "$pattern" | sort)
    if [ "${#BINS[@]}" -eq 0 ]; then
        echo "No test binaries match pattern $pattern in $BIN_DIR" >&2
        exit 1
    fi
    echo "Pushing ${#BINS[@]} binaries to $TARGET:$REMOTE_DIR …"
    ssh "$TARGET" "mkdir -p $REMOTE_DIR && rm -f $REMOTE_DIR/*"
    for b in "${BINS[@]}"; do
        scp -q "$BIN_DIR/$b" "$TARGET:$REMOTE_DIR/$b"
    done
}

# Run all tests in a single binary on the target.  $1=binary basename, $2=identity,
# $3=extra env (string like 'WATCHDOG_LAB_DANGEROUS=YES_REALLY')
run_binary() {
    local bin="$1" identity="$2" extra_env="${3:-}"
    echo "----- $bin (identity=$identity) -----"
    ssh -t "$TARGET" \
        "WATCHDOG_TEST_IDENTITY='$identity' ${extra_env:+$extra_env }sudo -E $REMOTE_DIR/$bin --test-threads=1 --nocapture --include-ignored" \
        || echo "FAILED: $bin (exit $?)"
}

# Run one exact test in a single binary.  Keep lab mode deterministic:
# libtest/serial_test prevents concurrency, but does not give us the
# ordering we need when one test intentionally reboots the target.
run_binary_test() {
    local bin="$1" identity="$2" test_name="$3" extra_env="${4:-}"
    echo "----- $bin::$test_name (identity=$identity) -----"
    ssh -t "$TARGET" \
        "WATCHDOG_TEST_IDENTITY='$identity' ${extra_env:+$extra_env }sudo -E $REMOTE_DIR/$bin '$test_name' --exact --test-threads=1 --nocapture --include-ignored"
}

# LAB-01 succeeds by rebooting the target, so the SSH client normally exits
# with 255 after "Broken pipe".  Treat that disconnect as the expected result
# and leave post-reboot evidence collection to capture-run.sh.
run_reboot_expected_test() {
    local bin="$1" identity="$2" test_name="$3"
    echo "----- $bin::$test_name (identity=$identity, reboot expected) -----"
    set +e
    ssh -t "$TARGET" \
        "WATCHDOG_TEST_IDENTITY='$identity' WATCHDOG_LAB_DANGEROUS=YES_REALLY sudo -E $REMOTE_DIR/$bin '$test_name' --exact --test-threads=1 --nocapture --include-ignored"
    local rc=$?
    set -e
    if [ "$rc" -eq 255 ]; then
        echo "EXPECTED-REBOOT: $bin::$test_name disconnected SSH (exit 255)"
    elif [ "$rc" -eq 0 ]; then
        echo "FAILED: $bin::$test_name returned normally; expected the target to reboot." >&2
        return 1
    else
        echo "FAILED: $bin::$test_name (exit $rc)" >&2
        return "$rc"
    fi
}

# Find the binary on the target whose filename starts with `<base>-`.
# Returns the basename (e.g. "softdog-9b7…") or empty.
find_remote_binary() {
    local base="$1"
    ssh "$TARGET" "ls $REMOTE_DIR/ 2>/dev/null | grep -E '^${base}-' | head -1"
}

# ---------------------------------------------------------------------------
# LAB MODE
# ---------------------------------------------------------------------------
if [ -n "$LAB_MODULE" ]; then
    cat <<EOF

================================================================================
  ATTENTION — LAB MODE will load $LAB_MODULE on $TARGET and run lab checks
  against its watchdog.  It runs the non-rebooting Magic-V check first, then
  the no-ping reboot check.  The final check may REBOOT the machine when the
  watchdog fires correctly.
  Press Ctrl-C in the next 5 seconds to abort.
================================================================================

EOF
    sleep 5

    # Discover identity by diffing /sys/class/watchdog/* across the
    # modprobe.  Robust to drivers that have no /device/driver symlink
    # (e.g. softdog, which is a pure misc-device driver), and to hosts
    # where the platform watchdog (sp5100_tco/iTCO/sbsa_gwdt) is
    # already auto-probed at boot — without this diff, the resolver
    # would mis-target the pre-existing platform watchdog.
    LAB_IDENTITY="$(ssh "$TARGET" "
        pre=\$(mktemp)
        post=\$(mktemp)
        trap 'rm -f \$pre \$post' EXIT
        ls -1 /sys/class/watchdog/ 2>/dev/null | sort -u >\$pre
        echo 'Loading kernel module $LAB_MODULE …' >&2
        sudo modprobe $LAB_MODULE || exit 11
        sleep 1
        ls -1 /sys/class/watchdog/ 2>/dev/null | sort -u >\$post
        new=\$(comm -13 \$pre \$post)
        n=\$(printf '%s\n' \"\$new\" | grep -c '^watchdog' || true)

        if [ \"\$n\" -eq 1 ]; then
            cat \"/sys/class/watchdog/\$new/identity\"
            exit 0
        fi

        if [ \"\$n\" -eq 0 ]; then
            # Module was already loaded before us — fall back to
            # /sys/class/watchdog/*/device/driver symlink match for
            # drivers that bind to a platform device.  Normalize by
            # stripping _ and - from both sides so '<mod>_rust' (module)
            # matches '<mod>-tco' or '<mod>-gwdt' (driver name in
            # /sys/bus/platform/drivers/).
            mod=\$(echo '$LAB_MODULE' | tr '-' '_')
            mod_norm=\$(echo \"\$mod\"        | tr -d '_-')
            mod_stripped_norm=\$(echo \"\${mod%_rust}\" | tr -d '_-')
            mod_stripped2_norm=\$(echo \"\${mod%_drv}\"  | tr -d '_-')
            for d in /sys/class/watchdog/watchdog*; do
                [ -e \"\$d/device/driver\" ] || continue
                drv=\$(basename \$(readlink -f \"\$d/device/driver\"))
                drv_norm=\$(echo \"\$drv\" | tr -d '_-')
                if [ \"\$drv_norm\" = \"\$mod_norm\" ] \\
                    || [ \"\$drv_norm\" = \"\$mod_stripped_norm\" ] \\
                    || [ \"\$drv_norm\" = \"\$mod_stripped2_norm\" ]; then
                    cat \"\$d/identity\"
                    exit 0
                fi
            done
            exit 12
        fi

        echo \"\$new\" >&2
        exit 13
    ")" || {
        rc=$?
        case $rc in
            11) echo "modprobe $LAB_MODULE failed; aborting." >&2 ;;
            12) echo "$LAB_MODULE was already loaded and no /device/driver symlink matched; rmmod the conflicting driver first or use the autonomous path." >&2 ;;
            13) echo "modprobe $LAB_MODULE produced multiple new watchdogs; cannot disambiguate. rmmod conflicting drivers first." >&2 ;;
            *)  echo "Could not discover identity for module $LAB_MODULE (rc=$rc)" >&2 ;;
        esac
        exit 1
    }
    if [ -z "$LAB_IDENTITY" ]; then
        echo "Could not discover identity for module $LAB_MODULE" >&2
        exit 1
    fi
    echo "  module $LAB_MODULE → identity \"$LAB_IDENTITY\""

    push_binaries '^lab_dangerous-'

    for b in "${BINS[@]}"; do
        if ! run_binary_test "$b" "$LAB_IDENTITY" "lab_02_magic_v_disarms" "WATCHDOG_LAB_DANGEROUS=YES_REALLY"; then
            echo "FAILED: $b::lab_02_magic_v_disarms; not running reboot test." >&2
            exit 1
        fi
        run_reboot_expected_test "$b" "$LAB_IDENTITY" "lab_01_no_ping_reboot"
    done
    exit 0
fi

# ---------------------------------------------------------------------------
# AUTONOMOUS MODE
# ---------------------------------------------------------------------------

# Snapshot pre-existing watchdog identities so cleanup knows what to
# leave alone.
echo "Enumerating pre-existing watchdogs …"
PRE_EXISTING="$(ssh "$TARGET" '
    for f in /sys/class/watchdog/*/identity; do
        [ -e "$f" ] && cat "$f"
    done 2>/dev/null
' || true)"
echo "  before modprobe: $(echo "$PRE_EXISTING" | tr '\n' ',' | sed 's/,$//' | sed 's/^$/<none>/')"

# Snapshot loaded modules so cleanup knows what to leave alone.
PRE_LOADED_MODULES="$(ssh "$TARGET" 'lsmod | awk "NR>1 {print \$1}"' || true)"

# Modprobe only the modules we have first-class tests for.  The
# previous design bulk-loaded everything under
# /lib/modules/.../kernel/drivers/watchdog/, which blew up on real
# hardware — several upstream drivers either auto-arm on probe (WDAT)
# or unconditionally set WATCHDOG_NOWAYOUT_INIT_STATUS.  Loading them
# without then driving them = box reboots.
#
# Drivers loaded at boot (iTCO_wdt, hpwdt, BIOS-managed WDAT, etc.)
# are still picked up by the post-modprobe enumerate step below and
# get basic conformance coverage via common_conformance.
# Force nowayout=0 explicitly.  Module-cmdline params override the
# kernel's build-time WATCHDOG_NOWAYOUT default, so this guarantees a
# stoppable timer regardless of how the running kernel was configured.
# Without this, on Kylin we observed nowayout=Y by default — which
# makes magic-V close a no-op, blocks SETOPTIONS DISABLECARD, and
# turns rmmod into a delayed reboot/panic.
# Arch-specific module list.  The "hardware" driver candidate is paired
# with softdog-rust so every supported arch ends up exercising at least
# two managed identities — a hardware-backed one (when probe succeeds)
# and the pure-software softdog (always works).  We do NOT try drivers
# from the wrong arch: sp5100_tco-rust on arm64 (or sbsa_gwdt-rust on
# x86_64) would just fail probe with -ENODEV, producing noise.
case "$ARCH" in
    x86_64)  MODULES_TO_LOAD="sp5100_tco-rust softdog-rust" ;;
    aarch64) MODULES_TO_LOAD="sbsa_gwdt-rust softdog-rust" ;;
    *)       MODULES_TO_LOAD="softdog-rust" ;;
esac

echo "Loading our managed watchdog drivers (nowayout=0, arch=$ARCH): $MODULES_TO_LOAD"
MODULES_LOADED_BY_US=""
for mod in $MODULES_TO_LOAD; do
    if ssh "$TARGET" "sudo modprobe $mod nowayout=0 2>/dev/null"; then
        echo "  modprobe $mod nowayout=0"
        mod_lsmod="${mod//-/_}"
        if ! grep -qx "$mod_lsmod" <<< "$PRE_LOADED_MODULES"; then
            MODULES_LOADED_BY_US="${MODULES_LOADED_BY_US}${mod_lsmod}"$'\n'
        fi
    else
        echo "  modprobe $mod failed (driver not applicable on this hardware — skipping)"
    fi
done
sleep 1

# Re-enumerate to get the authoritative test list.
echo "Re-enumerating /sys/class/watchdog after bulk modprobe …"
IDENTITIES_RAW="$(ssh "$TARGET" '
    for f in /sys/class/watchdog/*/identity; do
        [ -e "$f" ] && cat "$f"
    done 2>/dev/null
' || true)"

if [ -z "$IDENTITIES_RAW" ]; then
    echo "No watchdog devices on target after bulk modprobe; nothing to test."
    exit 0
fi

mapfile -t IDENTITIES <<< "$IDENTITIES_RAW"
echo "  testing ${#IDENTITIES[@]} watchdog(s):"
for id in "${IDENTITIES[@]}"; do
    if per_driver_binary_for_identity "$id" >/dev/null; then
        echo "    - \"$id\" (known: full per-driver suite)"
    else
        echo "    - \"$id\" (unknown: basic conformance only)"
    fi
done

# Push every binary the autonomous path may need.
push_binaries '^(common_conformance|common_extended|gc_test|sbsa_gwdt|softdog|sp5100_tco)-'

# Cache resolved remote binary names so we don't re-`ls` for each identity.
COMMON_CONF_BIN="$(find_remote_binary common_conformance)"
COMMON_EXT_BIN="$(find_remote_binary common_extended)"
GC_TEST_BIN="$(find_remote_binary gc_test)"

# Iterate identities.  Per-driver test binaries that don't match the
# current identity skip themselves cleanly via skip_unless_identity.
for id in "${IDENTITIES[@]}"; do
    echo
    echo "============================================================"
    echo "Testing identity: \"$id\""
    echo "============================================================"
    [ -n "$COMMON_CONF_BIN" ] && run_binary "$COMMON_CONF_BIN" "$id"
    [ -n "$COMMON_EXT_BIN" ]  && run_binary "$COMMON_EXT_BIN"  "$id"

    if base="$(per_driver_binary_for_identity "$id")"; then
        bin="$(find_remote_binary "$base")"
        if [ -n "$bin" ]; then
            run_binary "$bin" "$id"
        else
            echo "WARN: no $base-* binary found in $REMOTE_DIR" >&2
        fi
    fi
    # gc_test: the 4-item end-to-end QA procedure (driver-agnostic).
    [ -n "$GC_TEST_BIN" ] && run_binary "$GC_TEST_BIN" "$id"
done

# ---------------------------------------------------------------------------
# Cleanup: rmmod the modules WE loaded.  Pre-existing modules (PCI auto-
# probed at boot, etc.) are left alone.
#
# Historical note: this step used to be omitted because the Rust
# sbsa_gwdt / softdog ports had a module-exit race that panicked the
# kernel on rmmod.  Both have been fixed in kernel commits:
#   80f0d9ea4d3 — sync-cancel hrtimers in softdog_exit_rust + ops.owner
#                  threading for try_module_get correctness
#   acba0d48d28a — thread THIS_MODULE through ops.owner & driver.owner
#                  for all Rust watchdog ports (incl. sp5100_tco)
# `gc_04_modprobe_cycle_x10` exercises 10x rmmod/modprobe per identity
# every run and would catch a regression immediately.
# ---------------------------------------------------------------------------
echo
if [ -n "$MODULES_LOADED_BY_US" ]; then
    echo "Cleanup: rmmod modules loaded by this run:"
    while IFS= read -r mod; do
        [ -z "$mod" ] && continue
        if ! ssh "$TARGET" "lsmod | awk 'NR>1 {print \$1}' | grep -qx '$mod'"; then
            echo "  $mod already unloaded"
        elif ssh "$TARGET" "sudo rmmod $mod 2>/dev/null"; then
            echo "  rmmod $mod"
        else
            echo "  rmmod $mod failed (still in use? — left loaded)"
        fi
    done <<< "$MODULES_LOADED_BY_US"
fi

echo
echo "Done."
