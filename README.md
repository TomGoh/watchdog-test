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

### 1. Build the test binaries

```bash
# One-time toolchain setup — pick whichever target arch matches your hardware.
rustup target add aarch64-unknown-linux-musl   # for ARM64 boards
rustup target add x86_64-unknown-linux-musl    # for x86_64 boxes

# Build a release-mode static-musl bundle.  Output: target/<triple>/release/deps/...
./scripts/build.sh aarch64                     # OR  ./scripts/build.sh x86_64
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

You will *also* need passwordless `sudo` on the target — the device
node `/dev/watchdog0` is `0660 root:root`, and `/dev/kmsg` is
`0440 root:root`.  Either:

- Add a *scoped* sudoers rule (recommended, restores nothing-for-free
  semantics elsewhere):

  ```bash
  ssh user@target-host
  sudo tee /etc/sudoers.d/watchdog-test >/dev/null <<EOF
  $(whoami) ALL=(root) NOPASSWD: /tmp/watchdog-test/*
  EOF
  sudo chmod 0440 /etc/sudoers.d/watchdog-test
  sudo visudo -c -f /etc/sudoers.d/watchdog-test   # expects: parsed OK
  ```

- Or grant blanket NOPASSWD (acceptable on a dedicated test box):

  ```bash
  echo "$(whoami) ALL=(ALL) NOPASSWD: ALL" \
      | sudo tee /etc/sudoers.d/$(whoami)-nopasswd
  ```

### 3. Run the suite

Three tiers, picked via the 4th positional arg:

```bash
# Default: fast tier, no reboots, ~5 seconds wall-clock.
./scripts/deploy.sh <TARGET>

# Specify arch and target a particular driver by identity.
./scripts/deploy.sh <TARGET> aarch64 "SBSA Generic Watchdog"

# Extended tier — non-destructive but slower (continuous feed, modprobe cycle).
./scripts/deploy.sh <TARGET> aarch64 "SBSA Generic Watchdog" extended

# Lab tier — DESTRUCTIVE (will reboot the target if the watchdog fires correctly).
./scripts/deploy.sh <TARGET> aarch64 "SBSA Generic Watchdog" lab
```

The script prints a 5-second confirmation banner before lab mode
actually fires; press Ctrl-C in that window to abort.

### 4. (Optional) Archive the run

`scripts/capture-run.sh` is a wrapper around `scripts/deploy.sh` that
**bundles a fresh dmesg snapshot, a target-side metadata dump, and
the full stdout of every test binary** into a timestamped
subdirectory under `logs/`.  Use it whenever you want to keep a
record of a run — for bug reports, post-mortems, "here's what good
looks like" baselines, etc.  Tests don't read these files; they're
written for humans.

```bash
./scripts/capture-run.sh <TARGET> aarch64 "SBSA Generic Watchdog" fast
```

The four positional arguments mirror `deploy.sh`:

| Position | Argument | Examples |
|---|---|---|
| 1 | SSH target | `kylin-pc`, `lab-arm-01`, `jose@192.0.2.42` |
| 2 | Target arch | `aarch64` (default), `x86_64` |
| 3 | Watchdog identity (the `WatchdogInfo::identity` string) | `"SBSA Generic Watchdog"`, `"Software Watchdog"`, `"SP5100 TCO Watchdog"` |
| 4 | Test tier | `fast` (default), `extended`, `lab` |

So a run dir's name (`logs/2026-05-08-kylin-pc-sbsa_generic_watchdog-fast-1533/`)
**encodes the exact command that produced it** — the args are
recoverable from the directory alone.

What ends up inside each run dir:

| File | Contents |
|---|---|
| `meta.txt` | target hostname, `uname -a`, `lsmod \| grep wdt` snapshot, `/sys/class/watchdog/*` field-by-field dump |
| `dmesg-pre.log` | watchdog-relevant `dmesg` lines BEFORE the run started (timestamps preserved) |
| `dmesg-post.log` | same filter AFTER the run finished |
| `dmesg-delta.log` | the diff — kernel-log lines this specific run caused |
| `tests-<tier>.log` | full stdout from each test binary in invocation order, including SKIP markers and per-test annotations |

Older runs are never overwritten — every `capture-run.sh` invocation
creates a fresh directory.  See [`logs/README.md`](logs/README.md)
for the full naming convention and reading order.

---

## Test tiers

| Tier | Mode | Reboot risk | Covers | Wall-clock |
|---|---|---|---|---|
| Fast | `fast` (default) | none | static state + single-shot ops + lifecycle (no slow loops) | ~5 s |
| Extended | `extended` | none | adds `--include-ignored` slow-but-safe tests (continuous-feed loop, modprobe cycle, …) | ~25 s |
| Lab | `lab` | **YES — will reboot the target** | `lab_dangerous-*` binary only, gated by `WATCHDOG_LAB_DANGEROUS=YES_REALLY` | ~15 s |

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
2. Drop a new `tests/<driver>.rs` with the per-driver invariants;
   start by copying the `softdog.rs` skeleton (it's already
   skip-aware).
3. (Optional) drop a `tests/<driver>_extended.rs` for slow / SETOPTIONS
   / modprobe tests — copy the structure of `sbsa_gwdt_extended.rs`.
4. Re-run `./scripts/build.sh` and `./scripts/deploy.sh`.

The deploy script discovers test binaries by name pattern, so the new
file's binary will be picked up automatically — no manual config to
edit.

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

### Real-reset tests are NOT auto-recovering

If `lab_01_no_ping_reboot` *fails* (i.e. the watchdog doesn't fire),
the test prints a hard failure and exits — but the driver is left in
an armed state with the watchdog still running.  Subsequent SSH
sessions will see the timer eventually expire and reboot anyway.
**Run the lab tier only on machines you can safely power-cycle.**

---

## Cross-arch builds

The same source compiles for x86_64 — switch the toolchain triple:

```bash
rustup target add x86_64-unknown-linux-musl
./scripts/build.sh x86_64
./scripts/deploy.sh some-x86-target x86_64 "SP5100 TCO Watchdog"
```

The `sp5100_tco`-specific tests run; `sbsa_gwdt` and `softdog`
identity-gated tests skip cleanly because their drivers aren't
loaded.

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
