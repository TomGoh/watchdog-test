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
| `dmesg-pre.log` | `ssh <TARGET> 'dmesg \| grep -iE "RUST\|…"'` BEFORE the run | watchdog-relevant kernel-log lines that already existed at the start (with original timestamps) |
| `tests.log` | full stdout of `./scripts/deploy.sh <TARGET>` (or `… --lab <module>`) | per-test-binary stdout in invocation order, including individual `test foo … ok / FAILED` lines, SKIP markers, and any `# probe:` / `# rust lifecycle:` annotations |
| `dmesg-post.log` | same grep, AFTER the tests finish | the same filter at the end of the run |
| `dmesg-delta.log` | `diff` of pre vs post | the kernel-log lines added by THIS test run — usually the most interesting view |
| `meta-post.txt` | snapshots taken AFTER the run | post-run `lsmod` and `/sys/class/watchdog/*` dump, so you can see what changed (modules loaded, devices appeared/disappeared) |

## How to read a run

If you're investigating "what happened on `N80`'s 2026-05-16
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

## Reproducing a run

The directory name encodes the exact invocation:

```bash
# logs/2026-05-16-N80-autonomous-0920/
./scripts/capture-run.sh N80

# logs/2026-05-16-N80-lab-sbsa_gwdt_rust-0920/
./scripts/capture-run.sh N80 --lab sbsa_gwdt-rust
```

Arch is autodetected from the target (`uname -m` over SSH).

## On the gitignored run-dirs

`.gitignore` excludes `logs/[0-9][0-9][0-9][0-9]-*/` so transient
debugging runs don't accumulate in the repo.  One baseline run
(`2026-05-16-N80-autonomous-0920/`) is committed via an exception
rule, as a reference for "what does a clean passing run look like
on a healthy kernel".

If a particular run is interesting enough to share, attach the
relevant files to a GitHub issue rather than committing the whole
directory.
