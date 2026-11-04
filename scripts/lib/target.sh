# SPDX-License-Identifier: GPL-2.0
#
# scripts/lib/target.sh — "how do I run a command on the target?"
#
# Sourced by deploy.sh and capture-run.sh.  The caller sets TARGET first:
#
#   TARGET=""        → LOCAL mode: the machine running the script IS the
#                      target.  Commands run through `bash -c`, binaries
#                      are copied with `cp`.  No sshd, no SSH keys needed.
#   TARGET=<dest>    → SSH mode: commands run through `ssh <dest>`,
#                      binaries are copied with `scp` (original behaviour).
#
# Everything target-side goes through the helpers below, so the two
# scripts never call ssh/scp directly.

if [ -z "${TARGET:-}" ]; then
    TARGET_MODE="local"
    TARGET_LABEL="$(hostname -s 2>/dev/null || hostname 2>/dev/null || echo localhost)"
    TARGET_DESC="this machine ($TARGET_LABEL, local mode)"
else
    TARGET_MODE="ssh"
    TARGET_LABEL="$TARGET"
    TARGET_DESC="$TARGET"
fi

# How to become root on the target.  In SSH mode we always go through
# sudo (unchanged behaviour).  In local mode, if the script itself is
# already root there is nothing to elevate — and minimal systems may
# not even have sudo installed.
if [ "$TARGET_MODE" = "local" ] && [ "$(id -u)" -eq 0 ]; then
    SUDO=""          # plain command
    SUDO_E=""        # command that must see the caller's env vars
    SUDO_N=""        # non-interactive
else
    SUDO="sudo"
    SUDO_E="sudo -E"
    SUDO_N="sudo -n"
fi

is_local() { [ "$TARGET_MODE" = "local" ]; }

# Run a shell command string on the target.
target_sh() {
    if is_local; then
        bash -c "$1"
    else
        ssh "$TARGET" "$1"
    fi
}

# Same, but with a TTY (SSH mode uses `ssh -t` so sudo can prompt and
# test output streams unbuffered).  Locally we already have the caller's
# terminal.
target_sh_tty() {
    if is_local; then
        bash -c "$1"
    else
        ssh -t "$TARGET" "$1"
    fi
}

# Copy a local file to an absolute path on the target.
target_put() {
    local src="$1" dst="$2"
    if is_local; then
        cp -f "$src" "$dst"
    else
        scp -q "$src" "$TARGET:$dst"
    fi
}

# Is the target reachable right now (non-interactive, short timeout)?
target_reachable() {
    if is_local; then
        return 0
    fi
    ssh -o ConnectTimeout=3 -o BatchMode=yes "$TARGET" 'true' 2>/dev/null
}

# Local mode, non-root: make sure sudo works before we start, so the run
# doesn't stop halfway on a password prompt (or, under `sudo -n`, fail
# silently).  Prompts once if needed; sudo then caches the credential.
target_ensure_sudo() {
    is_local || return 0
    [ -z "$SUDO" ] && return 0
    if ! command -v sudo >/dev/null 2>&1; then
        echo "Local mode needs root: either install sudo or run this script as root." >&2
        return 1
    fi
    if ! sudo -n true 2>/dev/null; then
        echo "Local mode: root is needed for modprobe / /dev/watchdog* / dmesg." >&2
        echo "Enter your sudo password once (or configure NOPASSWD, see README):" >&2
        sudo -v || { echo "sudo authentication failed; aborting." >&2; return 1; }
    fi
}
