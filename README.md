# watchdog-test

Cross-platform Rust test suite for kernel watchdog drivers. Validates
the Rust-ported drivers (`softdog`, `sp5100_tco`, `sbsa_gwdt`, …)
against the kernel's watchdog uapi using a single static ELF that
you `scp` to the target machine and run.

The suite has been validated end-to-end against the Rust `sbsa_gwdt`
port on real ARMv8 hardware: **31/31 fast-tier tests pass, 2/2
lab-tier tests prove both the reset path and the clean-shutdown
path.**

---

## How this works (at a glance)

```
   ┌──────────────────────┐                  ┌──────────────────────────┐
   │ Build host           │   ssh + scp      │ Target                   │
   │ (anywhere with       │ ───────────────▶ │ (machine running the     │
   │  rustup + cargo)     │                  │  Rust watchdog kernel)   │
   │                      │ ◀─ test stdout ─ │                          │
   │ ./scripts/build.sh   │                  │ /tmp/watchdog-test/*     │
   │ ./scripts/deploy.sh  │                  │ ↑ runs as root via sudo  │
   └──────────────────────┘                  └──────────────────────────┘
```

The **build host** is wherever you run `cargo` from — your laptop,
a CI runner, a dev VM.  It produces static-musl test binaries.

The **target** is the machine actually running the kernel under test
— the box with `/dev/watchdog0` and the Rust driver loaded.  It
needs `sshd` listening and your build-host user reachable via SSH key
auth.

The build host and the target **can be the same machine** (just SSH
to `localhost`), but they usually aren't — most kernel work involves
cross-compiling on a developer workstation and pushing builds onto a
separate test box.

### The `<TARGET>` placeholder in this document

Throughout the rest of this README the literal string `<TARGET>`
stands for **whatever SSH destination you'd type after `ssh`** to
reach your target machine.  The deploy / capture scripts pass it
through directly; they don't care what form it's in.  Examples:

| What you'd put in place of `<TARGET>` | Why |
|---|---|
| `kylin-pc.lan` | bare hostname resolvable via DNS / mDNS / `/etc/hosts` |
| `192.0.2.42` | direct IP |
| `jose@192.0.2.42` | username + IP (when local + remote users differ) |
| `lab-arm-01` | an alias defined in `~/.ssh/config` (recommended) |
| `localhost` | when build host and target are the same machine |

For the rest of the document, all command examples use `<TARGET>` —
substitute your own value when running them.  If `ssh <TARGET> hostname`
works on your build host, the test scripts will work too.

---

## Table of contents

- [What you need](#what-you-need)
- [How it's structured](#how-its-structured)
- [Quick start](#quick-start)
  - [1. Build the test binaries](#1-build-the-test-binaries)
  - [2. Set up SSH access to your target](#2-set-up-ssh-access-to-your-target)
  - [3. Run the suite](#3-run-the-suite)
  - [4. (Optional) Archive the run](#4-optional-archive-the-run)
- [Test tiers](#test-tiers)
- [What's covered](#whats-covered)
- [Adding a new driver](#adding-a-new-driver)
- [Hardware quirks worth knowing](#hardware-quirks-worth-knowing)
- [Cross-arch builds](#cross-arch-builds)
- [Out of scope](#out-of-scope)

---

## What you need

| | Build host (where you compile) | Target (where the watchdog runs) |
|---|---|---|
| OS | any Linux with `rustup` | Linux with the kernel built from a `rust-watchdog-*` branch |
| Tooling | `cargo`, `rustup target add aarch64-unknown-linux-musl` (or `x86_64-…`) | `bash`, `sudo`, `ssh` server |
| Permissions | normal user | needs to run the test binaries as root (see [SSH setup](#2-set-up-ssh-access-to-your-target)) |
| Network | needs SSH access to the target | accepts SSH from the build host |

The build host and target can be the same machine. They can also be
different architectures — `cargo build --target aarch64-unknown-linux-musl`
on an x86 build host is supported.

---

## How it's structured

```
watchdog-test/
├── Cargo.toml                               # workspace root
├── crates/
│   ├── wdctl/                               # type-safe watchdog uapi wrapper (nix ioctls)
│   ├── tests-common/                        # shared assertions, skip helpers, lab consent gate
│   └── tests/
│       ├── tests/
│       │   ├── common_conformance.rs        # C-01..C-10 — every driver must pass (FAST)
│       │   ├── common_extended.rs           # C-EXT-01..05 — cross-driver lifecycle (FAST)
│       │   ├── sbsa_gwdt.rs                 # SBSA basic identity / format / clk (FAST)
│       │   ├── sbsa_gwdt_extended.rs        # SBSA continuous feed / SETOPTIONS / modprobe
│       │   ├── softdog.rs                   # auto-skips when softdog isn't loaded
│       │   ├── sp5100_tco.rs                # auto-skips when sp5100_tco isn't loaded
│       │   └── lab_dangerous.rs             # opt-in DESTRUCTIVE tier (lab only)
│       └── src/lib.rs                       # placeholder
├── scripts/
│   ├── build.sh                             # build static-musl binaries for chosen arch
│   ├── deploy.sh                            # scp + ssh-run on a target, mode-aware
│   └── capture-run.sh                       # archive a complete run under logs/
└── logs/                                    # archived run records (timestamps preserved)
    └── 2026-05-08-…/                        # one subdir per capture-run.sh invocation
```

---

## Quick start

The placeholder `<TARGET>` below stands for whatever **your** SSH
target is (an alias from `~/.ssh/config`, an `IP`, a `user@host`,
etc.) — see [SSH setup](#2-set-up-ssh-access-to-your-target) below.

### 1. (Optional) Pre-install the rustup target

The deploy script autodetects the target arch over SSH and triggers a
build automatically if no cached binaries exist.  You only need this
step if you want to pre-warm the build:

```bash
rustup target add aarch64-unknown-linux-musl   # for ARM64 boards
rustup target add x86_64-unknown-linux-musl    # for x86_64 boxes
./scripts/build.sh                              # uses host arch by default
```

The build produces several test binaries (one per `tests/*.rs` source
file), all linked statically against musl libc so they run on any
Linux of the matching arch with no glibc-version concerns.

### 2. Set up SSH access to your target

The deploy/capture scripts use plain `ssh` to reach the target, so
**any SSH alias or hostname your shell already recognises will
work** — there's nothing watchdog-test-specific about the connection.

If you don't yet have key-based passwordless SSH:

```bash
# Generate a key (skip if you already have one)
ssh-keygen -t ed25519 -C "$(whoami)@$(hostname)"

# Push it to the target
ssh-copy-id user@target-host
# Verify
ssh user@target-host 'hostname'
```

Optionally drop a host alias into `~/.ssh/config` so you can use a
short name in the test commands:

```
# ~/.ssh/config
Host my-watchdog-target
    HostName 192.0.2.42
    User jose
    IdentityFile ~/.ssh/id_ed25519
```

You will *also* need passwordless `sudo` on the target.  The
autonomous path needs root for three things: opening `/dev/watchdog0`
(0660 root:root), reading `/dev/kmsg` (0440 root:root), and
`modprobe` / `rmmod` of the watchdog modules.  The simplest setup is
a blanket NOPASSWD entry on a dedicated test box:

```bash
echo "$(whoami) ALL=(ALL) NOPASSWD: ALL" \
    | sudo tee /etc/sudoers.d/$(whoami)-nopasswd
sudo chmod 0440 /etc/sudoers.d/$(whoami)-nopasswd
```

If you want a tighter sudoers rule on a developer workstation,
include both the test binary path and the module-management commands:

```bash
sudo tee /etc/sudoers.d/watchdog-test >/dev/null <<EOF
$(whoami) ALL=(root) NOPASSWD: /tmp/watchdog-test/*, /sbin/modprobe, /sbin/rmmod, /usr/sbin/modprobe, /usr/sbin/rmmod
EOF
sudo chmod 0440 /etc/sudoers.d/watchdog-test
sudo visudo -c -f /etc/sudoers.d/watchdog-test   # expects: parsed OK
```

### 3. Run the suite

The autonomous path takes a single argument — the SSH target.  It
autodetects arch over SSH, bulk-loads every watchdog driver in the
target's kernel tree, then iterates `/sys/class/watchdog/*` running
the appropriate test set per discovered identity.

```bash
# Autonomous, reboot-safe.  Discovers + tests every loadable watchdog
# (sbsa_gwdt-drv, sp5100_tco-drv, softdog-drv, plus any third-party
# drivers like iTCO_wdt the kernel happens to ship).
./scripts/deploy.sh <TARGET>

# Lab tier — DESTRUCTIVE.  Loads ONLY the named module and runs the
# real-reboot lab tests against its watchdog.
./scripts/deploy.sh <TARGET> --lab sbsa_gwdt-drv
./scripts/deploy.sh <TARGET> --lab softdog-drv
```

Lab mode prints a 5-second confirmation banner before firing; press
Ctrl-C in that window to abort.

**What gets run per discovered identity:**
- **Known driver** (`sbsa_gwdt-drv` / `sp5100_tco-drv` / `softdog-drv`)
  — `common_conformance` + `common_extended` + the per-driver binary,
  all with `--include-ignored` (full extended coverage).
- **Unknown driver** (anything else that registered a `/sys/class/watchdog/*`)
  — `common_conformance` + `common_extended` only (basic uapi
  conformance check).

After the run the script `rmmod`s every module it loaded and leaves
pre-existing modules untouched.

### 4. (Optional) Archive the run

`scripts/capture-run.sh` is a wrapper around `scripts/deploy.sh` that
**bundles a fresh dmesg snapshot, a target-side metadata dump, and
the full stdout of every test binary** into a timestamped
subdirectory under `logs/`.  Use it whenever you want to keep a
record of a run — for bug reports, post-mortems, "here's what good
looks like" baselines, etc.  Tests don't read these files; they're
written for humans.

```bash
./scripts/capture-run.sh <TARGET>                  # autonomous run
./scripts/capture-run.sh <TARGET> --lab softdog-drv # lab run
```

Arguments mirror `deploy.sh`:

| Argument | Description |
|---|---|
| `<TARGET>` | SSH destination (alias, IP, or `user@host`) |
| `--lab <module>` | Optional: run the destructive lab tier against the named kernel module |

Run-directory name encodes the kind of run:

- Autonomous: `logs/<YYYY-MM-DD>-<host>-autonomous-<HHMM>/`
- Lab: `logs/<YYYY-MM-DD>-<host>-lab-<sanitized-module>-<HHMM>/`

What ends up inside each run dir:

| File | Contents |
|---|---|
| `meta.txt` | pre-run target hostname, `uname -a`, `lsmod` snapshot, `/sys/class/watchdog/*` dump |
| `meta-post.txt` | same snapshots taken AFTER the run (so you can see what changed) |
| `dmesg-pre.log` | watchdog-relevant `dmesg` lines BEFORE the run |
| `dmesg-post.log` | same filter AFTER the run |
| `dmesg-delta.log` | the diff — kernel-log lines this specific run caused |
| `tests.log` | full stdout from `deploy.sh` (one section per discovered identity) |

Older runs are never overwritten — every `capture-run.sh` invocation
creates a fresh directory.  See [`logs/README.md`](logs/README.md)
for the full naming convention and reading order.

---

## Test tiers

The deploy script exposes two modes:

| Mode | Invocation | Reboot risk | Covers | Wall-clock |
|---|---|---|---|---|
| Autonomous | `./scripts/deploy.sh <TARGET>` | none — by construction (every test goes through `with_open(...)` which always magic-V closes) | every loadable watchdog driver in the target's kernel tree, with full extended coverage (`--include-ignored`) per discovered identity | ~30–60 s per identity |
| Lab | `./scripts/deploy.sh <TARGET> --lab <module>` | **YES — will reboot the target** when the watchdog fires correctly | `lab_dangerous-*` only, gated by `WATCHDOG_LAB_DANGEROUS=YES_REALLY` and identity-locked to the named module | ~15 s |

Internally the test crate still uses the fast/extended split via
`#[ignore]` — `cargo test` locally runs only the basic tests; `cargo
test -- --include-ignored` runs everything.  The deploy script always
includes ignored tests.

Skip semantics: every per-driver test (`softdog_*`, `sp5100_*`, …)
prints a `# SKIP: identity X is not Y` marker and counts as a pass
when the running kernel doesn't have that driver loaded.  Lab tests
likewise skip with a marker if the consent env var isn't set.

---

## What's covered

For `sbsa_gwdt` specifically (validated on real hardware):

| Surface | Tests | Validates |
|---|---|---|
| Identity string | sbsa_01, c02 | `WatchdogInfo::identity` |
| Options bitmap | sbsa_02, c03 | `WDIOF_*` flags claimed by the driver |
| Format-stable dmesg "Initialized" line | sbsa_03 | `wrapper_dev_info_init_log` C helper |
| arch_timer Hz plausible | sbsa_04, sbsa_ext_06 | `safe_arch_timer_get_cntfrq()` Rust shim |
| `WDIOC_KEEPALIVE` ioctl | c05 | Rust `sbsa_gwdt_keepalive` op |
| `write(fd, "x", 1)` ping | sbsa_ext_02, c_ext_01 | Same op, different code path |
| `WDIOC_SETTIMEOUT` round-trip | c07, sbsa_ext_03 | Rust `set_timeout` op |
| `WDIOC_SETTIMEOUT` clamping | sbsa_ext_03, c_ext_03 | WOR programming math |
| `WDIOC_SETTIMEOUT(0)` rejected | c08 | Min-timeout enforcement |
| `WDIOC_GETTIMELEFT` value | c09 | Rust `get_timeleft` op |
| `WDIOC_GETTIMELEFT` progresses | c_ext_05 | `safe_arch_timer_read_counter()` |
| `WDIOC_GETBOOTSTATUS` | c_ext_04 | bootstatus byte plumbing |
| `WDIOC_SETOPTIONS DISABLE/ENABLE` | sbsa_ext_04 | Rust `start`/`stop` via direct ioctl |
| Magic-V close clean | c06 | Watchdog core close path |
| No-V close keeps timer running | sbsa_ext_05 | Driver doesn't auto-stop |
| Concurrent open EBUSY | c_ext_02 | `watchdog_dev.c` exclusivity |
| 30 s continuous feed | sbsa_ext_01 | End-to-end ping/keepalive over time |
| rmmod / modprobe cycle | sbsa_ext_99 | `init_rust` ↔ `exit_rust` lifecycle |
| `[RUST]` lifecycle log lines | c10 | `pr_info!` / `dev_info!` macros |
| **Real reboot on no-ping** | lab_01 *(destructive)* | Hardware reset path WS0→WS1 |
| **Magic-V actually disarms** | lab_02 *(destructive on failure)* | `sbsa_gwdt_stop()` clears WCS |

---

## Adding a new driver

1. Add the identity string to `tests/common_conformance.rs`'s
   `c10_rust_lifecycle_log` dispatch.
2. Drop a new `tests/<driver>.rs` with the per-driver invariants —
   copy the structure of `softdog.rs` or `sbsa_gwdt.rs`.  Basic tests
   at the top, slow / invasive ones below the divider with `#[ignore]`.
3. Add two case arms to `scripts/deploy.sh` so the autonomous path
   recognises the new identity:
   ```bash
   per_driver_binary_for_identity() {
       case "$1" in
           ...
           "Your New Watchdog") echo "your_new_driver" ;;  # NEW
       esac
   }
   ```
   And extend the binary-name pattern in the `push_binaries` call:
   ```bash
   push_binaries '^(common_conformance|common_extended|sbsa_gwdt|softdog|sp5100_tco|your_new_driver)-'
   ```
4. Re-run `./scripts/build.sh` and `./scripts/deploy.sh <TARGET>`.

If the kernel module ships under
`/lib/modules/$(uname -r)/kernel/drivers/watchdog/`, the autonomous
path will bulk-modprobe it automatically — no per-driver loading
logic in the script.

---

## Hardware quirks worth knowing

These are surprises we've already hit and resolved.  If you hit
unexpected behaviour, check this list before assuming a bug.

### `bootstatus` after a watchdog reset is firmware-dependent

The SBSA spec says `WCS_WS1` should be sticky across hardware reset,
so the kernel can detect "we got here because the watchdog fired" and
set `WDIOF_CARDRESET` (bit 0x20) in `bootstatus`.  In practice,
**firmware on many ARMv8 server / desktop platforms (including
KylinOS / Phytium boards we've tested) clears `WCS_WS1` during
platform init**, so `bootstatus` reads `0` even after a confirmed
watchdog-induced reset.

The lab-tier tests therefore do NOT assert on `WDIOF_CARDRESET` —
instead, the CI runner is expected to observe externally that the SSH
session dropped, the target became reachable again, and `uptime`
reports a fresh boot.

### `watchdog: watchdog0: watchdog did not stop!` is informational

When userspace closes `/dev/watchdog0` without writing the magic 'V'
byte and the driver was loaded with `nowayout=0`, the kernel emits
this warning but **transparently keeps pinging the watchdog on
userspace's behalf** (see `drivers/watchdog/watchdog_dev.c:
watchdog_release`).  The system survives.  This is what
`sbsa_ext_05_close_without_v_keeps_running` exercises; the warning
is the expected signature.

### `/dev/watchdog0` is exclusive

The watchdog core enforces single-opener semantics: a second `open()`
returns `EBUSY`.  The suite uses `serial_test::serial(watchdog)` to
serialise tests *within* one binary, but you can't run two test
binaries concurrently against the same device.

### Lab-tier consent gate

`lab_dangerous-*` tests refuse to fire unless
`WATCHDOG_LAB_DANGEROUS=YES_REALLY` is set in the environment.
`./scripts/deploy.sh <TARGET> ... lab` sets this for you; running the
binary directly via `cargo test` will not.  Even with
`cargo test -- --ignored`, the consent gate prevents accidental
reboots.

### Non-lab tests must stay reboot-safe by construction

The autonomous path runs every test against every loaded watchdog,
including drivers that *will* reset the box on close-without-V.  This
is safe today because every test goes through
`tests_common::with_open(...)` which always magic-V closes (even on
assertion failure), and the one test that deliberately closes without
'V' (`<driver>_ext_05`) re-opens within 1.5 s and magic-V closes well
inside the arm window.

**When adding a test:** if it leaves a `Watchdog` armed without a
follow-up magic-V close, it belongs in `lab_dangerous.rs`, NOT in a
per-driver file.  Breaking this invariant would turn the autonomous
path into a destructive path.

### Real-reset tests are NOT auto-recovering

If `lab_01_no_ping_reboot` *fails* (i.e. the watchdog doesn't fire),
the test prints a hard failure and exits — but the driver is left in
an armed state with the watchdog still running.  Subsequent SSH
sessions will see the timer eventually expire and reboot anyway.
**Run the lab tier only on machines you can safely power-cycle.**

---

## Cross-arch builds

Arch is autodetected from the target — `./scripts/deploy.sh
<TARGET>` does the right thing whether `<TARGET>` is ARM or x86.  The
deploy script will trigger `./scripts/build.sh <arch>` for the right
triple if no cached binaries exist.

To pre-build for both arches:

```bash
rustup target add aarch64-unknown-linux-musl x86_64-unknown-linux-musl
./scripts/build.sh aarch64
./scripts/build.sh x86_64
```

On x86 targets the autonomous path picks up `sp5100_tco-drv` (AMD
chipsets), `iTCO_wdt` (Intel), `softdog-drv`, etc.; on ARM targets it
picks up `sbsa_gwdt-drv`, `softdog-drv`, and any board-specific
drivers in the kernel tree.

---

## Out of scope

Deliberately not covered by this suite:

- **Suspend/resume**: requires `systemctl suspend` working reliably
  on the target.  Risk of bricking a developer workstation.  Better
  tested on a dedicated lab box.
- **`nowayout=1` enforcement**: requires the module to be loaded with
  `nowayout=1`, which means `rmmod` + `modprobe sbsa_gwdt-drv
  nowayout=1` mid-suite.  Achievable but invasive; would belong in
  the lab tier with its own consent gate.
- **Two-stage `action=1` IRQ + panic path**: would require kernel
  cmdline `sbsa_gwdt.action=1` at boot and a serial-console capture
  of the panic message before the WS1 reset hits.  Belongs in a
  bespoke CI runner.
