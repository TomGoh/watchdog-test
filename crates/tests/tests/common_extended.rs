// SPDX-License-Identifier: GPL-2.0
//! Extended cross-driver conformance tests — non-destructive, but
//! exercise lifecycle / state-machine surfaces the basic
//! `common_conformance.rs` skips in the interest of speed.
//!
//! All tests here run against whichever driver
//! [`tests_common::pick_watchdog`] resolves to (set
//! `WATCHDOG_TEST_IDENTITY` to target a specific driver in a
//! multi-watchdog system).

use std::time::Duration;

use anyhow::Result;
use serial_test::serial;
use tests_common::{describe, pick_watchdog, require_root, with_open};
use wdctl::options::*;

// ----------------------------------------------------------------------------
// C-EXT-01  Write-byte ping is accepted (the userspace-traditional ping
//           code path, distinct from WDIOC_KEEPALIVE).
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c_ext_01_write_byte_ping() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    println!("# probe: {}", describe(&sys)?);
    let mut wdt = sys.open_dev()?;
    wdt.write_byte_ping()?;
    // Sanity: timeleft should be in [1, timeout] after a ping.
    let left = wdt.timeleft()?;
    let max = sys.timeout()? as i32;
    assert!(
        (1..=max + 1).contains(&left),
        "post-ping timeleft {} not in (0, {}]",
        left,
        max
    );
    let _ = wdt.magic_close();
    Ok(())
}

// ----------------------------------------------------------------------------
// C-EXT-02  Concurrent open returns EBUSY — the watchdog core enforces
//           single-opener semantics (drivers/watchdog/watchdog_dev.c).
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c_ext_02_concurrent_open_ebusy() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    let path = sys.dev_node();

    // First opener holds the device for the duration of the test.
    let first = wdctl::Watchdog::open(&path)?;

    // Second open must fail.  Don't use the safe constructor (which
    // would treat the failure as a hard error) — go through OpenOptions
    // so we can inspect the errno.
    let r = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path);
    match r {
        Ok(_) => anyhow::bail!("second open of {} unexpectedly succeeded", path.display()),
        Err(e) => {
            let raw = e.raw_os_error().unwrap_or(0);
            // Linux returns EBUSY (16) for the second open.  Some
            // builds may return EAGAIN (11) instead under heavy load;
            // accept either.
            assert!(
                raw == libc::EBUSY || raw == libc::EAGAIN,
                "second open returned errno {} ({}), expected EBUSY/EAGAIN",
                raw,
                e
            );
        }
    }
    let _ = first.magic_close();
    Ok(())
}

// ----------------------------------------------------------------------------
// C-EXT-03  WDIOC_SETTIMEOUT > max_hw_heartbeat → kernel clamps and
//           returns the actual value, not the requested one.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c_ext_03_set_timeout_clamps_oversize() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    with_open(&sys, |w| {
        let info = w.info()?;
        if info.options & SETTIMEOUT == 0 {
            println!("# SKIP: driver does not advertise WDIOF_SETTIMEOUT");
            return Ok(());
        }
        let original = sys.timeout()? as i32;
        // Pick something well past anything reasonable.  3600s = 1h is
        // larger than any in-tree watchdog's max_hw_heartbeat.
        let oversize = 3600;
        match w.set_timeout(oversize) {
            Ok(actual) => {
                assert!(
                    actual <= oversize,
                    "kernel returned {} > requested {} (no clamp?)",
                    actual,
                    oversize
                );
                let echoed = sys.timeout()? as i32;
                assert_eq!(actual, echoed, "post-clamp ioctl/sysfs mismatch");
            }
            Err(_) => {
                // Some drivers reject oversize outright (-EINVAL) instead
                // of clamping; that's also valid kernel behaviour.
                println!("# note: driver rejects oversize SETTIMEOUT outright");
            }
        }
        let _ = w.set_timeout(original);
        Ok(())
    })
}

// ----------------------------------------------------------------------------
// C-EXT-04  WDIOC_GETBOOTSTATUS reads cleanly and reports a sane bitmap
//           (typically 0 if the previous boot wasn't watchdog-triggered).
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c_ext_04_bootstatus_readable() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    with_open(&sys, |w| {
        let bs = w.boot_status()?;
        // Only WDIOF_* bits are valid here — verify no garbage in the
        // upper half.
        let valid_mask: i32 = (OVERHEAT
            | FANFAULT
            | EXTERN1
            | EXTERN2
            | POWERUNDER
            | CARDRESET
            | POWEROVER
            | SETTIMEOUT
            | MAGICCLOSE
            | PRETIMEOUT
            | ALARMONLY
            | KEEPALIVEPING) as i32;
        assert!(
            bs & !valid_mask == 0,
            "bootstatus 0x{:08x} has bits outside WDIOF_* mask 0x{:08x}",
            bs,
            valid_mask
        );
        println!("# bootstatus = 0x{:04x}", bs);
        Ok(())
    })
}

// ----------------------------------------------------------------------------
// C-EXT-05  GETTIMELEFT decreases over wall-clock — proves the kernel
//           is actually counting and our get_timeleft op returns
//           something time-derived rather than constant.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn c_ext_05_timeleft_progresses() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    with_open(&sys, |w| {
        // Ping to reset the timer to a known position.
        w.keep_alive()?;
        let t1 = w.timeleft()?;
        std::thread::sleep(Duration::from_millis(1500));
        let t2 = w.timeleft()?;
        // Allow ±1s slop for arch_timer rounding.
        let dropped = t1 - t2;
        assert!(
            (1..=3).contains(&dropped),
            "timeleft delta {} not in [1,3] after 1.5s sleep (t1={}, t2={})",
            dropped,
            t1,
            t2
        );
        Ok(())
    })
}
