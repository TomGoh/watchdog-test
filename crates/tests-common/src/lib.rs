// SPDX-License-Identifier: GPL-2.0
//! Shared helpers used by the per-driver test suites:
//!
//! - [`require_root`] — early-skip non-root invocations with a clear msg
//! - [`pick_watchdog`] — resolve the watchdog under test by identity, or
//!   fall back to /dev/watchdog0 when `WATCHDOG_TEST_IDENTITY` env var
//!   isn't set
//! - [`dmesg_eventually`] — poll `/dev/kmsg` for an expected substring
//!   with a timeout (kernel emits dmesg asynchronously after some
//!   ioctls — naive `dmesg_find` immediately after a syscall can race)
//! - [`assert_options_supports`] — assert a WDIOF_* bitmap is reflected
//!   in `info.options`

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use wdctl::{dmesg_snapshot, enumerate, identity_str, WatchdogInfo, WatchdogSysfs};

/// Skip the test if not running as root.  Returns an Err so `?` in the
/// test body bails out cleanly.
pub fn require_root() -> Result<()> {
    // SAFETY: getuid() is always safe to call.
    let uid = unsafe { libc::getuid() };
    if uid != 0 {
        bail!("test requires root (current uid = {uid}); skipping");
    }
    Ok(())
}

/// Find the [`WatchdogSysfs`] entry under test.
///
/// Resolution order:
/// 1. `$WATCHDOG_TEST_IDENTITY` — exact identity-string match
/// 2. First entry with `/dev/watchdog0`
/// 3. First entry returned by enumerate()
pub fn pick_watchdog() -> Result<WatchdogSysfs> {
    let entries = enumerate()?;
    if entries.is_empty() {
        bail!("no /sys/class/watchdog/watchdog* — is a watchdog driver loaded?");
    }
    if let Ok(want) = std::env::var("WATCHDOG_TEST_IDENTITY") {
        for e in &entries {
            if let Ok(id) = e.identity() {
                if id == want {
                    return Ok(e.clone());
                }
            }
        }
        bail!("no watchdog with identity {want:?}");
    }
    if let Some(zero) = entries.iter().find(|e| e.index == 0) {
        return Ok(zero.clone());
    }
    Ok(entries.into_iter().next().unwrap())
}

/// Poll dmesg for `needle` until `timeout` elapses.  Returns Ok on first
/// match, Err if not found in time.
pub fn dmesg_eventually(needle: &str, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(line) = dmesg_snapshot()?.into_iter().find(|l| l.contains(needle)) {
            return Ok(line);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(anyhow!("dmesg never produced a line containing {needle:?}"))
}

/// Assert that `info.options` has every bit in `required` set.
pub fn assert_options_supports(info: &WatchdogInfo, required: u32, what: &str) -> Result<()> {
    let missing = required & !info.options;
    if missing == 0 {
        Ok(())
    } else {
        bail!(
            "{what}: required options bitmap 0x{:08x} missing 0x{:08x} \
             (driver advertises 0x{:08x})",
            required, missing, info.options,
        )
    }
}

/// Open the device node, run a closure with the [`Watchdog`], then
/// magic-V close so the watchdog actually stops.  This is the
/// recommended pattern for any test that needs to talk to /dev/watchdog0
/// without leaving the timer running.
pub fn with_open<F, T>(sys: &WatchdogSysfs, f: F) -> Result<T>
where
    F: FnOnce(&wdctl::Watchdog) -> Result<T>,
{
    let wdt = sys
        .open_dev()
        .with_context(|| format!("open {}", sys.dev_node().display()))?;
    let result = f(&wdt);
    // Always magic-close so we don't leave the watchdog armed.  On
    // failure we still want to release the device.
    let _ = wdt.magic_close();
    result
}

/// Per-driver tests use this guard so they short-circuit to a clean
/// pass (printing a `# SKIP:` marker) when the running kernel doesn't
/// have the driver under test loaded.
///
/// `cargo test` has no native skip semantics, so the canonical pattern
/// is to return `Ok(None)` and let the call site `let-else` it.
pub fn skip_unless_identity(want: &str) -> Result<Option<WatchdogSysfs>> {
    let sys = pick_watchdog()?;
    let id = sys.identity()?;
    if id != want {
        println!("# SKIP: identity {id:?} is not {want:?}");
        return Ok(None);
    }
    Ok(Some(sys))
}

/// Lab-CI consent gate. Tests that *will* reboot the system on success
/// call this first; without `WATCHDOG_LAB_DANGEROUS=YES_REALLY` set in
/// the environment, they print a SKIP marker and return Ok(()) so an
/// accidental `cargo test -- --ignored` doesn't power-cycle the box.
pub fn require_lab_consent() -> Result<bool> {
    match std::env::var("WATCHDOG_LAB_DANGEROUS") {
        Ok(v) if v == "YES_REALLY" => Ok(true),
        _ => {
            println!(
                "# SKIP: lab-CI test requires WATCHDOG_LAB_DANGEROUS=YES_REALLY \
                 (this test will REBOOT the machine on success)"
            );
            Ok(false)
        }
    }
}

/// Convenience to dump some context when a test fails.
pub fn describe(sys: &WatchdogSysfs) -> Result<String> {
    let info = sys.open_dev()?.info()?;
    Ok(format!(
        "watchdog{} identity={:?} options=0x{:04x} timeout={:?}s nowayout={:?}",
        sys.index,
        identity_str(&info),
        info.options,
        sys.timeout().ok(),
        sys.nowayout().ok(),
    ))
}
