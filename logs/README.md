# `logs/` — archived test runs

Every invocation of `scripts/capture-run.sh` creates a new
subdirectory here.  These are **archival records of past test
runs**, not actively asserted against by any test code — their job
is to preserve "what did the kernel actually emit / what did each
test print, on this machine, on this date" for debugging,
onboarding, and bug-report attachments.

## Directory naming convention

```
logs/<YYYY-MM-DD>-<host-label>-<driver-slug>-<tier>-<HHMM>/
```

| Component | What it encodes | Examples |
|---|---|---|
| `<YYYY-MM-DD>` | Calendar date the run started, build-host local time | `2026-05-08` |
| `<host-label>` | The `<TARGET>` arg, with `/`, `:`, `.` replaced by `_` | `kylin-pc_lan`, `192_0_2_42`, `lab-arm-01` |
| `<driver-slug>` | The `--identity` arg, lowercased and `_`-separated | `sbsa_generic_watchdog`, `sp5100_tco_watchdog` |
| `<tier>` | Which test tier was run | `fast`, `extended`, `lab` |
| `<HHMM>` | Local-time hours and minutes the run started | `1533` |

Older runs are never overwritten — every `capture-run.sh` invocation
creates a fresh directory.

## What's inside each run dir

| File | Source | Contents |
|---|---|---|
| `meta.txt` | written by `capture-run.sh` | run metadata: build-host invocation args, target hostname (`uname -a`), `lsmod` filtered to `wdt|watchdog`, snapshot of `/sys/class/watchdog/*` (identity / timeout / state / nowayout / bootstatus) |
| `dmesg-pre.log` | `ssh <TARGET> 'dmesg \| grep -iE "RUST\|sbsa-gwdt\|…"'` BEFORE running the tests | watchdog-relevant kernel-log lines that already existed at the start (with original timestamps) |
| `dmesg-post.log` | same grep, AFTER the tests finish | the same filter at the end of the run |
| `dmesg-delta.log` | `diff` of pre vs post | the kernel-log lines added by THIS test run — usually the most interesting view |
| `tests-<tier>.log` | full stdout of `./scripts/deploy.sh <TARGET> <arch> "<identity>" <tier>` | per-test-binary stdout in invocation order, including individual `test foo … ok / FAILED` lines, SKIP markers, and any `# probe:` / `# rust lifecycle:` annotations |

## How to read a run

If you're investigating "what happened on `kylin-pc`'s 2026-05-08
fast-tier run", open the directory and look in this order:

1. `meta.txt` first — confirm the run targeted the box and driver
   you think it did, and that the kernel version is what you expect.
2. `tests-fast.log` next — see which tests ran, which passed, which
   were skipped (per-driver tests skip cleanly when the driver isn't
   loaded).
3. `dmesg-delta.log` to see the kernel-side observable effects of
   the run — every `[RUST]` lifecycle line, every
   `Initialized with X timeout @ Y Hz` reformat, every
   `watchdog: watchdog0: watchdog did not stop!` warning.
4. If something looks wrong in (3), cross-reference against
   `dmesg-pre.log` to confirm the line wasn't pre-existing noise.

## Reproducing a run

The directory name encodes everything you need to reproduce it:

```bash
# logs/2026-05-08-kylin-pc-sbsa_generic_watchdog-fast-1533/
#                ^^^^^^^^ ^^^^^^^^^^^^^^^^^^^^^^^^ ^^^^
#                target   driver-slug              tier
./scripts/capture-run.sh kylin-pc aarch64 "SBSA Generic Watchdog" fast
```

(The arch isn't in the directory name — defaults to `aarch64`; pass
`x86_64` explicitly if running on x86 hardware.)
