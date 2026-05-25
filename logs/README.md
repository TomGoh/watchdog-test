# `logs/` — archived test runs + bug reports

Two kinds of content live here:

- **`bug-reports/`** — markdown reports describing driver bugs the
  suite has found.  Version-controlled.
- **`<YYYY-MM-DD>-<host>-…/`** — per-run snapshots produced by
  `scripts/capture-run.sh`.  Mostly gitignored (one baseline kept for
  reference); useful at debug time, not version-controlled noise.

These run-dir snapshots are **archival records of past test runs**,
not actively asserted against by any test code — their job is to
preserve "what did the kernel actually emit / what did each test
print, on this machine, on this date" for debugging, onboarding, and
bug-report attachments.

## Directory naming convention

Two shapes, matching `capture-run.sh`'s two invocation modes:

```
logs/<YYYY-MM-DD>-<host-label>-autonomous-<HHMM>/        # default
logs/<YYYY-MM-DD>-<host-label>-lab-<module-slug>-<HHMM>/  # --lab <module>
```

| Component | What it encodes | Examples |
|---|---|---|
| `<YYYY-MM-DD>` | Calendar date the run started, build-host local time | `2026-05-16` |
| `<host-label>` | The `<TARGET>` arg, with `/`, `:`, `.` replaced by `_` | `N80`, `kylin-pc_lan`, `192_0_2_42` |
| `autonomous` / `lab-<module-slug>` | Run kind.  `autonomous` is the default reboot-safe path; `lab-<slug>` is a `--lab <module>` destructive run. | `autonomous`, `lab-sbsa_gwdt_rust`, `lab-softdog_rust` |
| `<HHMM>` | Local-time hours and minutes the run started | `0920` |

Older runs are never overwritten — every `capture-run.sh` invocation
creates a fresh directory.

## What's inside each run dir

| File | Source | Contents |
|---|---|---|
| `meta.txt` | written by `capture-run.sh` BEFORE the run | run kind, target hostname, `uname -a`, `uptime`, pre-run `lsmod` filtered to `wdt\|watchdog\|softdog`, pre-run snapshot of `/sys/class/watchdog/*` (identity / timeout / pretimeout / state / nowayout / bootstatus) |
| `dmesg-pre.log` | `ssh <TARGET> 'sudo -n dmesg'` filtered locally BEFORE the run | watchdog-relevant kernel-log lines that already existed at the start (with original timestamps). Uses non-interactive sudo because some targets restrict direct `dmesg` access. |
| `tests.log` | full stdout of `./scripts/deploy.sh <TARGET>` (or `… --lab <module>`) | per-test-binary stdout in invocation order, including individual `test foo … ok / FAILED` lines, SKIP markers, and any `# probe:` / `# rust lifecycle:` annotations |
| `dmesg-post.log` | same sudo dmesg capture and local filter, AFTER the tests finish | the same filter at the end of the run |
| `dmesg-delta.log` | `diff` of pre vs post | the kernel-log lines added by THIS test run — usually the most interesting view |
| `meta-post.txt` | snapshots taken AFTER the run | post-run `lsmod` and `/sys/class/watchdog/*` dump, so you can see what changed (modules loaded, devices appeared/disappeared) |

## How to read a run

If you're investigating "what happened on `N80`'s 2026-05-25
autonomous run", open the directory and look in this order:

1. `meta.txt` first — confirm the run targeted the box you think
   it did, and that the kernel version (`uname -a`) is what you
   expect.
2. `tests.log` next — see which test binaries ran, against which
   identities, which tests passed / failed / skipped.  Output is in
   sections per discovered identity (e.g. `SBSA Generic Watchdog`
   then `Software Watchdog (Rust)`).
3. `dmesg-delta.log` to see the kernel-side observable effects of
   the run — every `[RUST]` lifecycle line, every
   `Initialized with X timeout @ Y Hz` reformat, every
   `watchdog: watchdog0: watchdog did not stop!` warning.  Cross-
   reference against `dmesg-pre.log` to confirm anything suspicious
   wasn't pre-existing noise.
4. `meta-post.txt` shows what modules / devices were left loaded
   after the run — useful when the autonomous path leaves modules
   loaded intentionally (see the script's "Note: leaving these
   modules loaded" output).

### Reading a `lab-*` run

`lab-*` directories are *destructive* — the watchdog actually fires and
reboots the target.  This changes what success looks like compared with
an `autonomous` run:

- `tests.log` first shows `lab_02_magic_v_disarms` passing, proving the
  Magic-V clean-close path does not reboot the target.  It then runs
  `lab_01_no_ping_reboot`; success for that final check is
  `client_loop: send disconnect: Broken pipe` (or another SSH
  disconnect) followed by `EXPECTED-REBOOT: ... disconnected SSH
  (exit 255)`.  The SSH connection died because the box was rebooted by
  the watchdog.
- `dmesg-delta.log` is much shorter than an autonomous run's (typically
  ~40–60 lines) — it captures the `arming watchdog`, `WCS_EN written`,
  and (on the boxes that print one) the imminent-reset banner, then
  cuts off mid-record when the reset hits.
- `meta-post.txt` reflects the *fresh boot* — uptime is seconds, no
  lab module loaded (capture-run waits for the box to come back via
  `until ssh … 'true'` before sampling).
- `dmesg-post.log` shows kernel messages from the *new* boot, not a
  continuation of the pre-run dmesg.  Useful for "did the reboot bring
  back a clean kernel?" but not for "what did the lab test print
  before the reset?" — that lives in `dmesg-delta.log`.

## Reproducing a run

The directory name encodes the exact invocation:

```bash
# logs/2026-05-16-N80-autonomous-0920/
./scripts/capture-run.sh N80

# logs/2026-05-16-N80-lab-sbsa_gwdt_rust-0920/
./scripts/capture-run.sh N80 --lab sbsa_gwdt-rust
```

Arch is autodetected from the target (`uname -m` over SSH).

## Committed baselines

`.gitignore` excludes `logs/[0-9][0-9][0-9][0-9]-*/` by default so
transient debugging runs don't accumulate in the repo.  A small set
of baseline runs is committed via `!logs/<dir>/` exception rules in
`.gitignore`, as a reference for "what does a clean passing run look
like on a healthy kernel" and "what does a successful destructive lab
run look like".

Current canonical set (2026-05-25), produced against the kernel that
includes the `softdog` / `sbsa_gwdt` / `sp5100_tco` Rust ports:

| Run dir | Type | Identities exercised | Notes |
|---|---|---|---|
| `2026-05-25-N80-autonomous-0906/` | autonomous | SBSA Generic Watchdog, Software Watchdog (Rust) | 60/60 pass, gc_03 SKIP by consent gate |
| `2026-05-25-Hygon-autonomous-0906/` | autonomous | SP5100 TCO timer, Software Watchdog (Rust) | 60/60 pass, gc_03 SKIP by consent gate |
| `2026-05-25-N80-lab-sbsa_gwdt_rust-0921/` | lab | SBSA Generic Watchdog | `lab_02_magic_v_disarms` passed, then `lab_01_no_ping_reboot` fired SBSA hardware reset; SSH dropped and the box came back |
| `2026-05-25-Hygon-lab-sp5100_tco_rust-0921/` | lab | SP5100 TCO timer | `lab_02_magic_v_disarms` passed, then `lab_01_no_ping_reboot` fired SP5100 hardware reset |
| `2026-05-25-N80-lab-softdog_rust-0924/` | lab | Software Watchdog (Rust) | `lab_02_magic_v_disarms` passed, then `lab_01_no_ping_reboot` fired softdog `emergency_restart` |
| `2026-05-25-Hygon-lab-softdog_rust-0924/` | lab | Software Watchdog (Rust) | same, on x86_64 |

If a debugging run is interesting enough to share but isn't a new
baseline, attach the relevant files to a GitHub issue rather than
committing the whole directory.
