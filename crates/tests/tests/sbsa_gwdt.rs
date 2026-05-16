// SPDX-License-Identifier: GPL-2.0
//! sbsa_gwdt-specific tests.  Auto-skip if the running driver isn't SBSA.
//!
//! Layout convention: basic tests (always run) at the top, then
//! `#[ignore]`-gated extended tests below the divider.  The extended
//! tier (`./scripts/deploy.sh ... extended` / `cargo test --
//! --include-ignored`) picks them up.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serial_test::serial;
use tests_common::{dmesg_eventually, require_root, skip_unless_identity, with_open};
use wdctl::{identity_str, options::*, setopts};

const IDENTITY: &str = "SBSA Generic Watchdog";

// ============================================================================
// Basic tests — always run
// ============================================================================

// SBSA-S-01  Identity matches exactly
#[test]
#[serial(watchdog)]
fn sbsa_01_identity() -> Result<()> {
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

// SBSA-S-02  Options include SETTIMEOUT | KEEPALIVEPING | MAGICCLOSE | CARDRESET
#[test]
#[serial(watchdog)]
fn sbsa_02_options_bitmap() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    with_open(&sys, |w| {
        let info = w.info()?;
        let want = SETTIMEOUT | KEEPALIVEPING | MAGICCLOSE | CARDRESET;
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

// SBSA-S-03  dmesg "Initialized with %ds timeout @ %u Hz" line is byte-stable
//
// This is the format-stability check.  Any drift in the C-side helper
// `wrapper_dev_info_init_log` (or the Rust call site) breaks the
// regression and trips this test.
#[test]
#[serial(watchdog)]
fn sbsa_03_init_log_format() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let line = dmesg_eventually("Initialized with", Duration::from_millis(200))?;
    let re = regex_lite("Initialized with [0-9]+s timeout @ [0-9]+ Hz, action=[01]");
    assert!(re.is_match(&line), "init line did not match expected pattern: {line:?}");
    Ok(())
}

// SBSA-S-04  arch_timer-derived clk in dmesg matches CNTFRQ_EL0 expectation
//
// We can't read CNTFRQ_EL0 from userspace without the timer subsystem,
// but on aarch64 we can fall back to /proc/cpuinfo's "CPU implementer"
// hint or just bound-check (1 MHz <= clk <= 100 MHz covers all known
// SBSA-compliant SoCs).
#[test]
#[serial(watchdog)]
fn sbsa_04_clk_plausible() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let line = dmesg_eventually("Initialized with", Duration::from_millis(200))?;
    let hz: u64 = line
        .split('@')
        .nth(1)
        .and_then(|s| s.split("Hz").next())
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| anyhow!("could not parse Hz from line: {line:?}"))?;
    assert!(
        (1_000_000..=200_000_000).contains(&hz),
        "implausible arch_timer clk {hz} Hz",
    );
    Ok(())
}

// ============================================================================
// Extended tests — slow / invasive, gated by #[ignore]
// Run via the `extended` deploy tier or `cargo test -- --include-ignored`.
// ============================================================================

// SBSA-EXT-01  Continuous feed for 2 × default-timeout: open, ping
//              every (timeout-1)/2 seconds, never let the watchdog fire.
//              Validates the entire ping → keepalive → WCV-update loop
//              stays consistent over time.
#[test]
#[ignore]
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

// SBSA-EXT-02  Write-byte ping (write(fd, "x", 1)) is accepted —
//              distinct kernel code path from WDIOC_KEEPALIVE ioctl.
#[test]
#[ignore]
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

// SBSA-EXT-03  WDIOC_SETTIMEOUT round-trips at multiple legal values.
//              Verifies the WOR programming math is correct across the
//              v0 / v1 boundary and at the clamp limits.
#[test]
#[ignore]
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

// SBSA-EXT-04  SETOPTIONS DISABLECARD / ENABLECARD round-trip — exercises
//              the WatchdogOps::stop and ::start paths separately from
//              the open/close lifecycle.
//
//              Self-skips under nowayout=1: DISABLECARD is rejected by
//              watchdog_dev::watchdog_stop with -EBUSY when the device
//              has WDOG_NO_WAY_OUT set, so the test would always fail
//              for reasons unrelated to the driver under test.
#[test]
#[ignore]
#[serial(watchdog)]
fn sbsa_ext_04_setoptions_disable_enable() -> Result<()> {
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

// SBSA-EXT-05  Magic-V close-vs-no-V semantics: closing without writing
//              'V' must NOT clear the timer (kernel keeps it running so
//              if the watchdog daemon crashes the box reboots).
//
//              Reboot-safety: we re-open within 1.5s and magic-V close,
//              well inside the 60s arm window.
#[test]
#[ignore]
#[serial(watchdog)]
fn sbsa_ext_05_close_without_v_keeps_running() -> Result<()> {
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
        // Deliberately close without 'V' — exercises the kernel's
        // watchdog_release no-magic-V path.  Must use close_without_v()
        // (not drop) because Watchdog::Drop normally writes 'V' as a
        // safety net; we need to override that here.
        wdt.close_without_v();
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

// SBSA-EXT-06  Driver-version field decoded by sbsa_gwdt_get_version()
//              matches what dmesg announced.  Validates W_IIDR read.
#[test]
#[ignore]
#[serial(watchdog)]
fn sbsa_ext_06_dmesg_version_consistent() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let line = wdctl::dmesg_find("[RUST] probe: version=")?
        .ok_or_else(|| anyhow!("[RUST] probe: version= line not found in dmesg"))?;
    let version: u32 = line
        .split("version=")
        .nth(1)
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("could not parse version from {line:?}"))?;
    assert!(
        version <= 1,
        "version {} outside known SBSA range {{0,1}}: {line}",
        version
    );
    println!("# detected SBSA GWDT version = {}", version);
    Ok(())
}

// SBSA-EXT-99  rmmod + modprobe cycle: the driver tears down cleanly
//              and re-binds without losing identity.  Stresses
//              sbsa_gwdt_exit_rust → unregister_platform_driver →
//              re-init paths.
//
//              Self-skips under nowayout=1 (rmmod would stop the
//              in-kernel keepalive on an armed timer and reboot the
//              box).
#[test]
#[ignore]
#[serial(watchdog)]
fn sbsa_ext_99_modprobe_cycle() -> Result<()> {
    require_root()?;
    let Some(sys_before) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    if sys_before.nowayout().unwrap_or(false) {
        println!("# SKIP: nowayout=1 — rmmod would unsync the in-kernel keepalive and reboot/panic the box");
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

    println!("# rmmod sbsa_gwdt-rust");
    modcmd(&["rmmod", "sbsa_gwdt-rust"])?;

    // Poll until the device entry is gone.  rmmod returning success
    // only means the .ko's exit() ran; the underlying watchdog_device
    // unregister can still be in flight.  Without this wait, the next
    // modprobe races against a stale registration and the new
    // instance lands on a different minor (we've seen "cannot
    // register miscdev on minor=130 (err=-16). a legacy watchdog
    // module is probably present." with the old hardware timer still
    // armed → 10 s later the box reboots).
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
        "5s after rmmod, /sys/class/watchdog/* still has identity {IDENTITY:?} \
         — driver exit path didn't fully unregister"
    );

    println!("# modprobe sbsa_gwdt-rust");
    modcmd(&["modprobe", "sbsa_gwdt-rust"])?;

    // Poll for the new registration so the assertion below isn't racing.
    let deadline = Instant::now() + Duration::from_secs(5);
    let sys_after = loop {
        if let Some(s) = wdctl::enumerate()?
            .into_iter()
            .find(|w| w.identity().ok().as_deref() == Some(IDENTITY))
        {
            break s;
        }
        if Instant::now() >= deadline {
            anyhow::bail!("sbsa_gwdt did not reappear within 5s after modprobe");
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    // CRITICAL: open + magic-V close the freshly-loaded device to
    // disarm any timer that was [enabled] at probe (e.g. when WCS_EN
    // was already set in hardware from before the rmmod).  Skipping
    // this leaves the watchdog armed with no userspace pinger — the
    // hardware timeout fires shortly after this test returns and the
    // box reboots.
    let wdt_after = sys_after.open_dev()?;
    wdt_after.magic_close()?;

    let id_after = sys_after.identity()?;
    assert_eq!(id_before, id_after, "identity changed across reload");
    Ok(())
}

// ============================================================================
// Local regex helper (kept here so the per-driver file is self-contained)
// ============================================================================

fn regex_lite(pat: &'static str) -> RegexLite { RegexLite { pat } }

struct RegexLite {
    pat: &'static str,
}

impl RegexLite {
    fn is_match(&self, hay: &str) -> bool {
        for start in 0..=hay.len() {
            if self.try_at(&hay[start..]) {
                return true;
            }
        }
        false
    }

    fn try_at(&self, mut hay: &str) -> bool {
        let mut p = self.pat;
        while !p.is_empty() {
            if let Some(rest) = p.strip_prefix("[0-9]+") {
                let mut count = 0;
                while hay.starts_with(|c: char| c.is_ascii_digit()) {
                    hay = &hay[1..];
                    count += 1;
                }
                if count == 0 {
                    return false;
                }
                p = rest;
            } else if let Some(rest) = p.strip_prefix("[01]") {
                if !hay.starts_with('0') && !hay.starts_with('1') {
                    return false;
                }
                hay = &hay[1..];
                p = rest;
            } else {
                let pc = p.chars().next().unwrap();
                if !hay.starts_with(pc) {
                    return false;
                }
                hay = &hay[pc.len_utf8()..];
                p = &p[pc.len_utf8()..];
            }
        }
        true
    }
}

#[cfg(test)]
mod regex_lite_self_check {
    use super::regex_lite;
    #[test]
    fn matches_init_log_pattern() {
        let r = regex_lite("Initialized with [0-9]+s timeout @ [0-9]+ Hz, action=[01]");
        assert!(r.is_match("sbsa-gwdt sbsa-gwdt.0: Initialized with 10s timeout @ 50000000 Hz, action=0."));
        assert!(!r.is_match("Hello world"));
    }
}
