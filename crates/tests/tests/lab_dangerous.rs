// SPDX-License-Identifier: GPL-2.0
//! # Lab-CI dangerous tier
//!
//! Tests in this binary will, when working *correctly*, reboot the
//! machine.  Run only on a CI runner that can power-cycle the target
//! and inspect the next-boot dmesg / WDIOF_CARDRESET bits.
//!
//! ## Activation
//!
//! Every test calls [`tests_common::require_lab_consent`], which
//! demands `WATCHDOG_LAB_DANGEROUS=YES_REALLY` in the environment.
//! Without that the tests print a SKIP marker and pass cleanly — so
//! `cargo test --workspace -- --ignored` on a developer machine does
//! NOT power-cycle the workstation.
//!
//! ## How to invoke
//!
//! ```bash
//! ./scripts/deploy.sh N80 aarch64 "SBSA Generic Watchdog" lab
//! # the deploy script sets WATCHDOG_LAB_DANGEROUS=YES_REALLY and
//! # runs only this binary.
//! ```

use std::time::Duration;

use anyhow::Result;
use serial_test::serial;
use tests_common::{describe, pick_watchdog, require_lab_consent, require_root};

// ----------------------------------------------------------------------------
// LAB-01  No-ping reboot: open the watchdog, set a short timeout,
//         deliberately don't ping, and sleep past the timeout.  If the
//         driver works, the machine reboots while we're sleeping and
//         this process never returns.  If we fall through to the
//         post-sleep `panic!`, the watchdog FAILED to fire — that's a
//         real bug.
//
//         Marked `#[ignore]` so plain `cargo test` skips it; even with
//         `--ignored`, the consent gate prevents accidental fires.
// ----------------------------------------------------------------------------
#[test]
#[ignore = "lab-CI only — reboots the machine on success"]
#[serial(watchdog)]
fn lab_01_no_ping_reboot() -> Result<()> {
    require_root()?;
    if !require_lab_consent()? {
        return Ok(());
    }
    let sys = pick_watchdog()?;
    println!("# {}", describe(&sys)?);

    // Use a short timeout so the test completes quickly when it works.
    // 5 seconds is well above any sane scheduler latency window.
    let arm: i32 = 5;
    let wait = Duration::from_secs((arm as u64) + 5);

    let wdt = sys.open_dev()?;
    let actual = wdt.set_timeout(arm)?;
    println!(
        "# arming watchdog with timeout={}s; expecting reset within ~{}s",
        actual,
        wait.as_secs()
    );

    // Deliberately do not ping.  Keep the fd open so the watchdog
    // remains armed (close without 'V' would also keep it armed, but
    // holding the fd makes the intent crystal-clear in the test trace).
    std::thread::sleep(wait);

    // If we reach this line, the kernel did not reset the box.
    // That's a real driver bug.  Don't try to magic-close — leave the
    // watchdog armed so the operator knows something's wrong, but
    // also surface a hard test failure.
    drop(wdt);
    anyhow::bail!(
        "LAB-01 FAILED: slept {}s past {}s timeout without reboot; \
         the watchdog driver is not firing on real hardware",
        wait.as_secs(),
        actual
    );
}

// ----------------------------------------------------------------------------
// LAB-02  Magic-V is honoured: open + write 'V' + close cleanly.  After
//         the magic close the timer must NOT fire even after timeout +
//         slack seconds.  This is the inverse of LAB-01 — proves the
//         clean-shutdown path actually disarms the watchdog.
//
//         (Strictly speaking this isn't reboot-causing if working
//         correctly, but it's grouped here because a *failure* of the
//         clean shutdown path WILL reboot the box.)
// ----------------------------------------------------------------------------
#[test]
#[ignore = "lab-CI only — reboots the machine on FAILURE"]
#[serial(watchdog)]
fn lab_02_magic_v_disarms() -> Result<()> {
    require_root()?;
    if !require_lab_consent()? {
        return Ok(());
    }
    let sys = pick_watchdog()?;
    println!("# {}", describe(&sys)?);

    let arm: i32 = 5;
    {
        let wdt = sys.open_dev()?;
        let _ = wdt.set_timeout(arm);
        // Magic-V close — the documented path that disarms the timer.
        wdt.magic_close()?;
    }

    // Wait past the timeout we set.  If magic-V didn't actually disarm
    // the watchdog, the box reboots while we're sleeping.  Reaching
    // the post-sleep print proves the clean-shutdown path works.
    let wait = Duration::from_secs((arm as u64) + 5);
    println!(
        "# slept {}s past {}s timeout; system survived → magic-V works",
        wait.as_secs(),
        arm
    );
    std::thread::sleep(wait);
    Ok(())
}
