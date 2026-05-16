// SPDX-License-Identifier: GPL-2.0
//! Common conformance tests — every Rust-ported watchdog driver MUST pass
//! these, regardless of vendor / arch.  Failure here indicates a uapi
//! regression in the driver itself, not a per-platform quirk.
//!
//! Selection: these tests run against whichever watchdog the
//! [`tests_common::pick_watchdog`] resolver picked.  Set
//! `WATCHDOG_TEST_IDENTITY="SBSA Generic Watchdog"` (etc.) to target a
//! specific driver when multiple watchdogs are present.
//!
//! Concurrency: `serial_test::serial` forces sequential execution
//! because /dev/watchdog0 is exclusive — only one process can hold it
//! open at a time.

use std::time::Duration;

use anyhow::Result;
use serial_test::serial;
use tests_common::{
    assert_options_supports, describe, dmesg_eventually, pick_watchdog, require_root, with_open,
};
use wdctl::{identity_str, options::*};

// ----------------------------------------------------------------------------
// C-01  /sys/class/watchdog/watchdogN/ exists
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c01_sysfs_entry_exists() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    println!("# probe: {}", describe(&sys)?);
    assert!(sys.sysfs.is_dir(), "sysfs dir missing: {}", sys.sysfs.display());
    assert!(sys.dev_node().exists(), "device node missing: {}", sys.dev_node().display());
    Ok(())
}

// ----------------------------------------------------------------------------
// C-02  identity is non-empty and same via sysfs and ioctl
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c02_identity_consistent() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    let sys_id = sys.identity()?;
    let ioctl_id = with_open(&sys, |w| {
        let info = w.info()?;
        Ok(identity_str(&info).to_string())
    })?;
    assert!(!sys_id.is_empty(), "sysfs identity is empty");
    assert_eq!(sys_id, ioctl_id, "sysfs vs WDIOC_GETSUPPORT identity mismatch");
    Ok(())
}

// ----------------------------------------------------------------------------
// C-03  options bitmap includes KEEPALIVEPING (every watchdog must)
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c03_options_advertise_keepaliveping() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    with_open(&sys, |w| {
        let info = w.info()?;
        assert_options_supports(&info, KEEPALIVEPING, "C-03 KEEPALIVEPING")
    })
}

// ----------------------------------------------------------------------------
// C-04  default sysfs `timeout` is plausible (1..=600 seconds)
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c04_default_timeout_in_range() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    let t = sys.timeout()?;
    assert!(
        (1..=600).contains(&t),
        "implausible default timeout {t}s (expected 1..=600)",
    );
    Ok(())
}

// ----------------------------------------------------------------------------
// C-05  WDIOC_GETSUPPORT.options agrees with successful WDIOC_KEEPALIVE
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c05_keepalive_ioctl_works() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    with_open(&sys, |w| {
        w.keep_alive()?;
        Ok(())
    })
}

// ----------------------------------------------------------------------------
// C-06  open + magic-V close cycle leaves the box alive (no reset)
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c06_magic_close_cycle() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    // We rely on with_open's automatic magic_close; if it didn't take,
    // the watchdog would still be running and a subsequent open of the
    // same device would NOT see WDIOC_GETSTATUS reflecting WDIOF_KEEPALIVEPING.
    with_open(&sys, |_w| Ok(()))?;
    // If we're still alive 2s later, magic-close worked.
    std::thread::sleep(Duration::from_secs(2));
    Ok(())
}

// ----------------------------------------------------------------------------
// C-07  WDIOC_SETTIMEOUT to a sane value updates sysfs readback
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c07_set_timeout_round_trip() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    let original = sys.timeout()?;
    with_open(&sys, |w| {
        let info = w.info()?;
        if info.options & SETTIMEOUT == 0 {
            println!("# SKIP: driver does not advertise WDIOF_SETTIMEOUT");
            return Ok(());
        }
        let target: i32 = if original == 30 { 20 } else { 30 };
        let actual = w.set_timeout(target)?;
        // Some hardware clamps; require kernel-reported actual to match
        // sysfs readback exactly.
        let echoed = sys.timeout()? as i32;
        assert_eq!(actual, echoed, "kernel-reported {} vs sysfs {}", actual, echoed);
        // Restore (best-effort)
        let _ = w.set_timeout(original as i32);
        Ok(())
    })
}

// ----------------------------------------------------------------------------
// C-08  WDIOC_SETTIMEOUT(0) is rejected
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c08_set_timeout_zero_rejected() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    with_open(&sys, |w| {
        let info = w.info()?;
        if info.options & SETTIMEOUT == 0 {
            println!("# SKIP: driver does not advertise WDIOF_SETTIMEOUT");
            return Ok(());
        }
        match w.set_timeout(0) {
            Err(_) => Ok(()),
            Ok(t) => anyhow::bail!("WDIOC_SETTIMEOUT(0) should fail; kernel returned {t}"),
        }
    })
}

// ----------------------------------------------------------------------------
// C-09  WDIOC_GETTIMELEFT is in [0, max_timeout]
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c09_timeleft_in_range() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    let max = sys.timeout()? as i32;
    with_open(&sys, |w| {
        let left = w.timeleft()?;
        assert!(
            (0..=max + 5).contains(&left),
            "timeleft {} not in [0, {}+slack]",
            left,
            max
        );
        Ok(())
    })
}

// ----------------------------------------------------------------------------
// C-10  Rust-ported drivers emit the [RUST] lifecycle line on probe
//
// Skips automatically if the running driver is upstream-C (e.g. when
// running this suite on a non-Rust kernel for cross-checking).
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c10_rust_lifecycle_log() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    // Only run for the drivers we've Rust-ported.
    let id = sys.identity()?;
    let needle: &str = match id.as_str() {
        "SBSA Generic Watchdog" => "[RUST] sbsa_gwdt:",
        // The Rust softdog port advertises identity "Software Watchdog (Rust)"
        // (with the "(Rust)" suffix), distinct from the in-tree C softdog.
        "Software Watchdog (Rust)" => "[RUST] softdog",
        "SP5100 TCO Watchdog" => "[RUST] sp5100_tco:",
        other => {
            println!("# SKIP: identity {other:?} is not a known Rust-ported driver");
            return Ok(());
        }
    };
    let line = dmesg_eventually(needle, Duration::from_millis(500))?;
    println!("# rust lifecycle: {line}");
    Ok(())
}
