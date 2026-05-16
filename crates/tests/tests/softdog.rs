// SPDX-License-Identifier: GPL-2.0
//! softdog-drv-specific tests.  Auto-skip if the running driver isn't
//! the Rust softdog port.
//!
//! Layout matches the per-driver convention used by `sbsa_gwdt.rs`:
//! basic tests (always run) at the top, `#[ignore]`-gated extended
//! tests below the divider.  The extended tier picks them up via
//! `--include-ignored`.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serial_test::serial;
use tests_common::{require_root, skip_unless_identity, with_open};
use wdctl::{identity_str, options::*, setopts};

// The Rust softdog port advertises identity "Software Watchdog (Rust)"
// (with the "(Rust)" suffix), distinct from the in-tree C softdog's
// "Software Watchdog".  All tests in this file gate on the Rust string.
const IDENTITY: &str = "Software Watchdog (Rust)";

// ============================================================================
// Basic tests — always run
// ============================================================================

// SOFTDOG-S-01  Identity matches exactly
#[test]
#[serial(watchdog)]
fn softdog_01_identity() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    with_open(&sys, |w| {
        let info = w.info()?;
        assert_eq!(identity_str(&info), IDENTITY);
        Ok(())
    })
}

// SOFTDOG-S-02  Options bitmap matches softdog-drv's exact claimed set:
//               KEEPALIVEPING | MAGICCLOSE | SETTIMEOUT.  Locks out
//               regressions where the Rust port loses a feature flag.
#[test]
#[serial(watchdog)]
fn softdog_02_options_advertised() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    with_open(&sys, |w| {
        let info = w.info()?;
        let want = KEEPALIVEPING | MAGICCLOSE | SETTIMEOUT;
        assert_eq!(
            info.options & want,
            want,
            "options 0x{:04x} missing 0x{:04x}",
            info.options,
            want & !info.options
        );
        Ok(())
    })
}

// SOFTDOG-S-03  dmesg has the [RUST] init marker line — confirms
//               softdog-drv probed via the Rust path, not a stale C
//               module that happens to share the identity string.
#[test]
#[serial(watchdog)]
fn softdog_03_dmesg_init_log() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let line = wdctl::dmesg_find("[RUST] softdog")?
        .ok_or_else(|| anyhow!("[RUST] softdog init line not found in dmesg"))?;
    println!("# softdog init line: {line}");
    Ok(())
}

// ============================================================================
// Extended tests — slow / invasive, gated by #[ignore]
// ============================================================================

// SOFTDOG-EXT-01  Continuous feed for 2 × default-timeout.
#[test]
#[ignore]
#[serial(watchdog)]
fn softdog_ext_01_continuous_feed() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };

    let timeout = sys.timeout()? as u64;
    let half = (timeout / 2).max(1);
    let runtime = Duration::from_secs(timeout * 2);

    println!(
        "# feeding softdog for {}s (timeout={}, ping every {}s)",
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

// SOFTDOG-EXT-02  Write-byte ping (write(fd, "x", 1)) is accepted.
#[test]
#[ignore]
#[serial(watchdog)]
fn softdog_ext_02_write_byte_ping() -> Result<()> {
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

// SOFTDOG-EXT-03  WDIOC_SETTIMEOUT round-trips at multiple legal values.
//                 softdog has no clamping logic in the [1, 65535] range,
//                 so every value should round-trip exactly.
#[test]
#[ignore]
#[serial(watchdog)]
fn softdog_ext_03_settimeout_matrix() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    with_open(&sys, |w| {
        let original = sys.timeout()? as i32;
        for &want in &[1i32, 5, 10, 60, 600] {
            let actual = w.set_timeout(want)?;
            let echoed = sys.timeout()? as i32;
            assert_eq!(
                actual, echoed,
                "SETTIMEOUT({}) → kernel-reported {} but sysfs reads {}",
                want, actual, echoed
            );
            assert_eq!(
                actual, want,
                "SETTIMEOUT({}) returned {}; softdog should round-trip exactly",
                want, actual
            );
        }
        let _ = w.set_timeout(original);
        Ok(())
    })
}

// SOFTDOG-EXT-04  SETOPTIONS DISABLECARD / ENABLECARD round-trip.
//
//                 Self-skips under nowayout=1 — DISABLECARD returns
//                 -EBUSY when WDOG_NO_WAY_OUT is set, same as SBSA.
#[test]
#[ignore]
#[serial(watchdog)]
fn softdog_ext_04_setoptions_disable_enable() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    if sys.nowayout().unwrap_or(false) {
        println!("# SKIP: nowayout=1 — DISABLECARD always returns EBUSY in this mode");
        return Ok(());
    }
    with_open(&sys, |w| {
        w.set_options(setopts::DISABLECARD)?;
        let state_disabled = sys.state()?;
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

// SOFTDOG-EXT-05  Magic-V close-vs-no-V semantics: closing without 'V'
//                 must NOT clear the timer.  Reboot-safety: re-opens
//                 within 1.5s and magic-V closes, well inside arm window.
#[test]
#[ignore]
#[serial(watchdog)]
fn softdog_ext_05_close_without_v_keeps_running() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };

    let original = sys.timeout()? as i32;
    let arm_secs: i32 = 60;
    {
        let wdt = sys.open_dev()?;
        let _ = wdt.set_timeout(arm_secs);
        let _ = wdt.magic_close();
    }

    {
        let wdt = sys.open_dev()?;
        drop(wdt); // close-without-V — kernel must KEEP THE TIMER RUNNING
    }

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

    let _ = wdt.set_timeout(original);
    let _ = wdt.magic_close();
    Ok(())
}

// SOFTDOG-EXT-06  Module-param visibility in dmesg.  The Rust softdog
//                 port emits `[RUST] softdog initialized: soft_margin=N
//                 soft_panic=N nowayout=N` on probe.  This test parses
//                 that line so a regression in the format (or its
//                 removal) is caught.
#[test]
#[ignore]
#[serial(watchdog)]
fn softdog_ext_06_module_param_visible() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let line = wdctl::dmesg_find("[RUST] softdog initialized: soft_margin=")?
        .ok_or_else(|| anyhow!(
            "[RUST] softdog initialized: soft_margin=... line not found in dmesg — \
             the softdog-rust port must emit a probe line of the form \
             `[RUST] softdog initialized: soft_margin=N soft_panic=N nowayout=N`."
        ))?;
    let soft_margin: u32 = line
        .split("soft_margin=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.trim_end_matches('s').parse().ok())
        .ok_or_else(|| anyhow!("could not parse soft_margin from {line:?}"))?;
    assert!(
        (1..=65535).contains(&soft_margin),
        "soft_margin {} outside legal [1, 65535] range: {line}",
        soft_margin
    );
    let nowayout = line
        .split("nowayout=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .ok_or_else(|| anyhow!("could not find nowayout= in {line:?}"))?;
    assert!(
        matches!(nowayout, "0" | "1"),
        "nowayout {:?} should be 0 or 1: {line}",
        nowayout
    );
    println!("# detected soft_margin={soft_margin}s nowayout={nowayout}");
    Ok(())
}

// SOFTDOG-EXT-99  rmmod + modprobe cycle: identity preserved across
//                 reload.  Ordered LAST so prior tests run before the
//                 brief teardown window.
//
//                 Self-skips under nowayout=1 — see sbsa_ext_99 for the
//                 same reasoning.  softdog's hrtimer doesn't reboot
//                 hardware, but the racy-unregister failure mode still
//                 applies and would surface as test flakes.
#[test]
#[ignore]
#[serial(watchdog)]
fn softdog_ext_99_modprobe_cycle() -> Result<()> {
    require_root()?;
    let Some(sys_before) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    if sys_before.nowayout().unwrap_or(false) {
        println!("# SKIP: nowayout=1 — rmmod-cycle test unsafe under nowayout");
        return Ok(());
    }
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

    println!("# rmmod softdog-rust");
    modcmd(&["rmmod", "softdog-rust"])?;

    // Poll until the device entry is gone — see the equivalent
    // comment in sbsa_gwdt.rs for why this matters.  softdog is
    // hrtimer-backed so the racy-rmmod failure mode is less
    // catastrophic, but the same defensive pattern keeps the test
    // deterministic.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if wdctl::enumerate()?
            .iter()
            .all(|w| w.identity().ok().as_deref() != Some(IDENTITY))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        wdctl::enumerate()?
            .iter()
            .all(|w| w.identity().ok().as_deref() != Some(IDENTITY)),
        "5s after rmmod, /sys/class/watchdog/* still has identity {IDENTITY:?}"
    );

    println!("# modprobe softdog-rust");
    modcmd(&["modprobe", "softdog-rust"])?;

    let deadline = Instant::now() + Duration::from_secs(5);
    let sys_after = loop {
        if let Some(s) = wdctl::enumerate()?
            .into_iter()
            .find(|w| w.identity().ok().as_deref() == Some(IDENTITY))
        {
            break s;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("softdog did not reappear within 5s after modprobe");
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    // Open + magic-V close the freshly-loaded device — same disarm
    // discipline as sbsa_ext_99.  Defensive even on softdog (whose
    // hrtimer doesn't actually keep ticking after device unregister).
    let wdt_after = sys_after.open_dev()?;
    wdt_after.magic_close()?;

    let id_after = sys_after.identity()?;
    assert_eq!(id_before, id_after, "identity changed across reload");
    Ok(())
}
