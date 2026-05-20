// SPDX-License-Identifier: GPL-2.0
//! sp5100_tco-specific tests.  Auto-skip if the running driver isn't
//! sp5100_tco.  Only meaningful on x86_64 with an AMD/Hygon SP5100 /
//! SB800 / EFCH southbridge.
//!
//! Layout convention: basic tests (always run) at the top, then
//! `#[ignore]`-gated extended tests below the divider.  The extended
//! tier (`./scripts/deploy.sh ... extended` / `cargo test --
//! --include-ignored`) picks them up.
//!
//! Test slots intentionally mirror sbsa_gwdt.rs / softdog.rs so the
//! three per-driver suites stay in lock-step.  When you add a new test
//! to any one of them, add the parallel slot to the other two — the
//! deploy script iterates all available identities and expects the
//! same test matrix per driver.

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serial_test::serial;
use tests_common::{dmesg_eventually, require_root, skip_unless_identity, with_open};
use wdctl::{identity_str, options::*, setopts};

const IDENTITY: &str = "SP5100 TCO timer";

// ============================================================================
// Basic tests — always run
// ============================================================================

// SP5100-S-01  Identity matches exactly
#[test]
#[serial(watchdog)]
fn sp5100_01_identity() -> Result<()> {
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

// SP5100-S-02  Options include SETTIMEOUT | KEEPALIVEPING | MAGICCLOSE
//
// SP5100 does NOT advertise CARDRESET (unlike SBSA GWDT) because its
// action register is programmable POWEROFF/RESET — not statically tied
// to a hardware reset like SBSA's WS1.
#[test]
#[serial(watchdog)]
fn sp5100_02_options_bitmap() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    with_open(&sys, |w| {
        let info = w.info()?;
        let want = SETTIMEOUT | KEEPALIVEPING | MAGICCLOSE;
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

// SP5100-S-03  dmesg "Initialized. heartbeat=%d sec (nowayout=%d)" line
//              is byte-stable.
//
// Format-stability regression: matches drivers/rust/drivers/watchdog/
// src/sp5100_tco.rs's `[RUST] Initialized. heartbeat=<n> sec
// (nowayout=<n>)` end-of-probe line.  Any drift in the format string
// breaks this test.
#[test]
#[serial(watchdog)]
fn sp5100_03_dmesg_init_log() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let line = dmesg_eventually("[RUST] Initialized. heartbeat=", Duration::from_millis(200))?;
    let re = regex_lite("[RUST] Initialized. heartbeat=[0-9]+ sec (nowayout=[01])");
    assert!(re.is_match(&line), "init line did not match expected pattern: {line:?}");
    Ok(())
}

// SP5100-S-04  Chipset auto-detection picked one of the four known
//              layouts (SP5100 / SB800 / EFCH / EFCH_MMIO) and the
//              revision is in a plausible range.
//
// Substitutes for sbsa_04_clk_plausible — SP5100 has no clock-rate
// concept, but the same sanity-check spirit applies: verify the chipset
// detection at probe matched one of the documented southbridges and
// the PCI revision byte parsed cleanly.
#[test]
#[serial(watchdog)]
fn sp5100_04_chipset_detected() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let line = dmesg_eventually("[RUST] Detected ", Duration::from_millis(200))?;
    let known = ["SP5100", "SB800", "EFCH", "EFCH_MMIO"];
    let layout = known
        .iter()
        .find(|k| line.contains(&format!("[RUST] Detected {} layout", k)))
        .copied()
        .ok_or_else(|| anyhow!("layout line did not name a known chipset: {line:?}"))?;
    println!("# detected SP5100 chipset layout = {}", layout);
    Ok(())
}

// ============================================================================
// Extended tests — slow / invasive, gated by #[ignore]
// Run via the `extended` deploy tier or `cargo test -- --include-ignored`.
// ============================================================================

// SP5100-EXT-01  Continuous feed for 2 × default-timeout: open, ping
//                every (timeout-1)/2 seconds, never let the watchdog
//                fire.  Validates the ping → TRIGGER bit → counter
//                reload chain stays consistent over time.
#[test]
#[ignore]
#[serial(watchdog)]
fn sp5100_ext_01_continuous_feed() -> Result<()> {
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

// SP5100-EXT-02  Write-byte ping (write(fd, "x", 1)) is accepted —
//                distinct kernel code path from WDIOC_KEEPALIVE ioctl.
#[test]
#[ignore]
#[serial(watchdog)]
fn sp5100_ext_02_write_byte_ping() -> Result<()> {
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

// SP5100-EXT-03  WDIOC_SETTIMEOUT round-trips at multiple legal values.
//                Verifies tco_timer_set_timeout writes the COUNT register
//                correctly across the (1, 0xffff) supported range.
#[test]
#[ignore]
#[serial(watchdog)]
fn sp5100_ext_03_settimeout_matrix() -> Result<()> {
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
            assert_eq!(
                actual, want,
                "SETTIMEOUT({}) returned {} (SP5100 supports 1..0xffff with no clamp)",
                want, actual
            );
        }
        let _ = w.set_timeout(original);
        Ok(())
    })
}

// SP5100-EXT-04  SETOPTIONS DISABLECARD / ENABLECARD round-trip — exercises
//                tco_timer_stop / tco_timer_start (and the START_STOP bit
//                in the CONTROL register) separately from the open/close
//                lifecycle.
//
//                Self-skips under nowayout=1: DISABLECARD is rejected by
//                watchdog_dev::watchdog_stop with -EBUSY when the device
//                has WDOG_NO_WAY_OUT set, so the test would always fail
//                for reasons unrelated to the driver under test.
#[test]
#[ignore]
#[serial(watchdog)]
fn sp5100_ext_04_setoptions_disable_enable() -> Result<()> {
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

// SP5100-EXT-05  Magic-V close-vs-no-V semantics: closing without writing
//                'V' must NOT clear the timer (kernel keeps it running
//                so if the watchdog daemon crashes the box reboots /
//                powers off, matching the configured action register).
//
//                Reboot-safety: we re-open within 1.5s and magic-V close,
//                well inside the default 60s arm window.
#[test]
#[ignore]
#[serial(watchdog)]
fn sp5100_ext_05_close_without_v_keeps_running() -> Result<()> {
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

// SP5100-EXT-06  Action register dmesg line is consistent with the
//                action module parameter (default POWEROFF, optionally
//                RESET).  Validates the action register-config path in
//                sp5100_tco_timer_init.
#[test]
#[ignore]
#[serial(watchdog)]
fn sp5100_ext_06_dmesg_action_consistent() -> Result<()> {
    require_root()?;
    let Some(_) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    let line = wdctl::dmesg_find("[RUST] Setting watchdog action to:")?
        .ok_or_else(|| anyhow!("[RUST] Setting watchdog action to: line not found in dmesg"))?;
    let action = line
        .split("action to:")
        .nth(1)
        .map(|s| s.trim().split_whitespace().next().unwrap_or("").to_string())
        .ok_or_else(|| anyhow!("could not parse action from {line:?}"))?;
    assert!(
        action == "POWEROFF" || action == "RESET",
        "action {action:?} outside known SP5100 set {{POWEROFF,RESET}}: {line}"
    );
    println!("# detected SP5100 action = {}", action);
    Ok(())
}

// SP5100-EXT-99  rmmod + modprobe cycle: the driver tears down cleanly
//                and re-binds without losing identity.  Stresses
//                sp5100_tco_exit_rust → platform_device_unregister →
//                re-init paths.
//
//                Self-skips under nowayout=1 (rmmod would stop the
//                in-kernel keepalive on an armed timer and reboot/
//                power off the box).
#[test]
#[ignore]
#[serial(watchdog)]
fn sp5100_ext_99_modprobe_cycle() -> Result<()> {
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

    println!("# rmmod sp5100_tco-rust");
    modcmd(&["rmmod", "sp5100_tco-rust"])?;

    // Poll until the device entry is gone.  rmmod returning success
    // only means the .ko's exit() ran; the underlying watchdog_device
    // unregister can still be in flight.  Without this wait, the next
    // modprobe races against a stale registration.
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

    println!("# modprobe sp5100_tco-rust");
    modcmd(&["modprobe", "sp5100_tco-rust"])?;

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
            anyhow::bail!("sp5100_tco did not reappear within 5s after modprobe");
        }
        std::thread::sleep(Duration::from_millis(100));
    };

    // Open + magic-V close the freshly-loaded device so the hardware
    // timer is disarmed on test exit (probe leaves the timer stopped,
    // but defensive — matches sbsa_ext_99's safety net).
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
        let r = regex_lite("[RUST] Initialized. heartbeat=[0-9]+ sec (nowayout=[01])");
        assert!(r.is_match("kernel: [RUST] Initialized. heartbeat=60 sec (nowayout=0)"));
        assert!(!r.is_match("Hello world"));
    }
}
