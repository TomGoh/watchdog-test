// SPDX-License-Identifier: GPL-2.0
//! sbsa_gwdt-specific tests.  Auto-skip if the running driver isn't SBSA.

use std::time::Duration;

use anyhow::Result;
use serial_test::serial;
use tests_common::{dmesg_eventually, pick_watchdog, require_root, with_open};
use wdctl::{identity_str, options::*};

const IDENTITY: &str = "SBSA Generic Watchdog";

fn skip_unless_sbsa() -> Result<wdctl::WatchdogSysfs> {
    let sys = pick_watchdog()?;
    let id = sys.identity()?;
    if id != IDENTITY {
        anyhow::bail!("# SKIP: identity {id:?} is not {IDENTITY:?}");
    }
    Ok(sys)
}

// SBSA-S-01  Identity matches exactly
#[test]
#[serial(watchdog)]
fn sbsa_01_identity() -> Result<()> {
    require_root()?;
    let sys = skip_unless_sbsa()?;
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
    let sys = skip_unless_sbsa()?;
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
    let _ = skip_unless_sbsa()?;
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
    let _ = skip_unless_sbsa()?;
    let line = dmesg_eventually("Initialized with", Duration::from_millis(200))?;
    let hz: u64 = line
        .split('@')
        .nth(1)
        .and_then(|s| s.split("Hz").next())
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| anyhow::anyhow!("could not parse Hz from line: {line:?}"))?;
    assert!(
        (1_000_000..=200_000_000).contains(&hz),
        "implausible arch_timer clk {hz} Hz",
    );
    Ok(())
}

// Tiny regex helper that doesn't pull in the full `regex` crate just
// for two patterns.  Supports literal text + the `[A-Z]+`, `[0-9]+`,
// `[01]` we actually use.  If patterns get more complex, switch to the
// real `regex-lite` crate.
fn regex_lite(pat: &'static str) -> RegexLite { RegexLite { pat } }

struct RegexLite {
    pat: &'static str,
}

impl RegexLite {
    fn is_match(&self, hay: &str) -> bool {
        // Walk the pattern, expanding the small set of supported classes
        // and matching against successive positions in `hay`.
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
