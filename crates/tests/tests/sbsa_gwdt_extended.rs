// SPDX-License-Identifier: GPL-2.0
//! sbsa_gwdt-specific extended tests — non-destructive but slow / more
//! invasive than the basic `sbsa_gwdt.rs` suite.  Auto-skip if the
//! running driver isn't SBSA.
//!
//! Notable test:
//!   `sbsa_ext_01_continuous_feed` runs for ~30 s of real wall-clock
//!   time — by far the slowest test in the suite, but the most
//!   meaningful proof that the watchdog actually services pings the
//!   way userspace expects.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serial_test::serial;
use tests_common::{require_root, skip_unless_identity, with_open};
use wdctl::setopts;

const IDENTITY: &str = "SBSA Generic Watchdog";

// ----------------------------------------------------------------------------
// SBSA-EXT-01  Continuous feed for 2 × default-timeout: open, ping
//              every (timeout-1)/2 seconds, never let the watchdog fire.
//              Validates the entire ping → keepalive → WCV-update loop
//              stays consistent over time.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn sbsa_ext_01_continuous_feed() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };

    let timeout = sys.timeout()? as u64;
    let half = (timeout / 2).max(1);
    let runtime = Duration::from_secs(timeout * 2);

    println!(
        "# feeding watchdog for {}s (timeout={}, ping every {}s)",
        runtime.as_secs(),
        timeout,
        half
    );

    let wdt = sys.open_dev()?;
    let start = Instant::now();
    let mut pings = 0u32;
    while start.elapsed() < runtime {
        wdt.keep_alive()?;
        pings += 1;
        // After each ping the timeleft should be at least `half` again.
        let left = wdt.timeleft()?;
        assert!(
            left as u64 >= half,
            "post-ping timeleft {} < half-period {}",
            left,
            half
        );
        std::thread::sleep(Duration::from_secs(half));
    }
    let _ = wdt.magic_close();
    println!("# survived {} pings over {}s", pings, runtime.as_secs());
    Ok(())
}

// ----------------------------------------------------------------------------
// SBSA-EXT-02  Write-byte ping (write(fd, "x", 1)) is accepted —
//              distinct kernel code path from WDIOC_KEEPALIVE ioctl.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn sbsa_ext_02_write_byte_ping() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let mut wdt = sys.open_dev()?;
    wdt.write_byte_ping()?;
    let left = wdt.timeleft()?;
    assert!(left > 0, "post-write-ping timeleft non-positive: {}", left);
    let _ = wdt.magic_close();
    Ok(())
}

// ----------------------------------------------------------------------------
// SBSA-EXT-03  WDIOC_SETTIMEOUT round-trips at multiple legal values.
//              Verifies the WOR programming math is correct across the
//              v0 / v1 boundary and at the clamp limits.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn sbsa_ext_03_settimeout_matrix() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    with_open(&sys, |w| {
        let original = sys.timeout()? as i32;
        for &want in &[1i32, 5, 10, 30, 60, 80] {
            let actual = match w.set_timeout(want) {
                Ok(v) => v,
                Err(_) => {
                    println!("# {}s rejected — likely past max_hw_heartbeat", want);
                    continue;
                }
            };
            let echoed = sys.timeout()? as i32;
            assert_eq!(
                actual, echoed,
                "SETTIMEOUT({}) → kernel-reported {} but sysfs reads {}",
                want, actual, echoed
            );
            // For values within the SBSA v0 max (~85 s on 50 MHz) the
            // kernel should accept exactly what we asked for.
            if want <= 80 {
                assert_eq!(
                    actual, want,
                    "SETTIMEOUT({}) returned {} (unexpected clamp inside v0 range)",
                    want, actual
                );
            }
        }
        let _ = w.set_timeout(original);
        Ok(())
    })
}

// ----------------------------------------------------------------------------
// SBSA-EXT-04  SETOPTIONS DISABLECARD / ENABLECARD round-trip — exercises
//              the WatchdogOps::stop and ::start paths separately from
//              the open/close lifecycle.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn sbsa_ext_04_setoptions_disable_enable() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    with_open(&sys, |w| {
        // Some drivers refuse SETOPTIONS without first being explicitly
        // started — we open which already starts via watchdog_dev.c, so
        // the disable/enable path should work.
        w.set_options(setopts::DISABLECARD)?;
        let state_disabled = sys.state()?;
        // sysfs `state` is "active" / "inactive"
        assert_eq!(
            state_disabled, "inactive",
            "after DISABLECARD, sysfs state expected \"inactive\", got {:?}",
            state_disabled
        );

        w.set_options(setopts::ENABLECARD)?;
        let state_enabled = sys.state()?;
        assert_eq!(
            state_enabled, "active",
            "after ENABLECARD, sysfs state expected \"active\", got {:?}",
            state_enabled
        );
        Ok(())
    })
}

// ----------------------------------------------------------------------------
// SBSA-EXT-05  Magic-V close-vs-no-V semantics: closing without writing
//              'V' must NOT clear the timer (kernel keeps it running so
//              if the watchdog daemon crashes the box reboots).
//
//              We arm with a generous timeout (60 s) and explicitly
//              measure the timer is still ticking afterwards.  Then we
//              re-open and magic-V close to leave the box safe.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn sbsa_ext_05_close_without_v_keeps_running() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };

    // Configure a known timeout.
    let original = sys.timeout()? as i32;
    let arm_secs: i32 = 60;
    {
        let wdt = sys.open_dev()?;
        let _ = wdt.set_timeout(arm_secs);
        let _ = wdt.magic_close();
    }

    // Open without magic-V close: drop the file handle without writing 'V'.
    {
        let wdt = sys.open_dev()?;
        // Explicit drop of `wdt` (no magic_close()) — kernel sees a
        // close-without-V and must KEEP THE TIMER RUNNING.
        drop(wdt);
    }

    // Now reopen, observe that timeleft has actually advanced past
    // initial timeout (i.e., the timer was not reset on the close).
    std::thread::sleep(Duration::from_millis(1500));
    let wdt = sys.open_dev()?;
    let left = wdt.timeleft()?;
    assert!(
        left < arm_secs,
        "after close-without-V + 1.5s sleep, timeleft {} should be < {}; \
         the kernel reset the timer on close (bug)",
        left,
        arm_secs
    );

    // Restore default timeout and clean magic-V close.
    let _ = wdt.set_timeout(original);
    let _ = wdt.magic_close();
    Ok(())
}

// ----------------------------------------------------------------------------
// SBSA-EXT-06  Driver-version field decoded by sbsa_gwdt_get_version()
//              matches what dmesg announced.  Validates W_IIDR read.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn sbsa_ext_06_dmesg_version_consistent() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    // Our driver emits `[INFO] [RUST] probe: version=N mediatek_quirk=…`.
    let line = wdctl::dmesg_find("[RUST] probe: version=")?
        .ok_or_else(|| anyhow!("[RUST] probe: version= line not found in dmesg"))?;
    let version: u32 = line
        .split("version=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("could not parse version from {line:?}"))?;
    // SBSA v0 (32-bit WOR) and v1 (48-bit WOR) are the only defined
    // values; anything else is a bug.
    assert!(
        version <= 1,
        "version {} outside known SBSA range {{0,1}}: {line}",
        version
    );
    println!("# detected SBSA GWDT version = {}", version);
    Ok(())
}

// ----------------------------------------------------------------------------
// SBSA-EXT-07  rmmod + modprobe cycle: the driver tears down cleanly
//              and re-binds without losing identity.  Stresses
//              sbsa_gwdt_exit_rust → unregister_platform_driver →
//              re-init paths.  Ordered LAST in the file so prior tests
//              run before the brief teardown window.
// ----------------------------------------------------------------------------
#[test]
#[serial(watchdog)]
fn sbsa_ext_99_modprobe_cycle() -> Result<()> {
    require_root()?;
    let Some(sys_before) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let id_before = sys_before.identity()?;

    fn modcmd(args: &[&str]) -> Result<()> {
        let out = Command::new(args[0]).args(&args[1..]).output()?;
        if !out.status.success() {
            anyhow::bail!(
                "{args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    println!("# rmmod sbsa_gwdt-drv");
    modcmd(&["rmmod", "sbsa_gwdt-drv"])?;
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !std::path::Path::new("/dev/watchdog0").exists()
            || wdctl::enumerate()?.iter().all(|w| w.identity().ok().as_deref() != Some(IDENTITY)),
        "after rmmod, no /sys/class/watchdog/* entry should still claim identity {IDENTITY:?}"
    );

    println!("# modprobe sbsa_gwdt-drv");
    modcmd(&["modprobe", "sbsa_gwdt-drv"])?;
    std::thread::sleep(Duration::from_millis(500));

    let id_after = wdctl::enumerate()?
        .into_iter()
        .find_map(|w| w.identity().ok())
        .ok_or_else(|| anyhow!("no watchdog reappeared after modprobe"))?;
    assert_eq!(id_before, id_after, "identity changed across reload");
    Ok(())
}
