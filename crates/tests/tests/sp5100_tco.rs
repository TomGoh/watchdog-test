// SPDX-License-Identifier: GPL-2.0
//! sp5100_tco-specific tests (placeholder).  Auto-skip if the running
//! driver isn't sp5100_tco.  Only meaningful on x86_64 with an
//! AMD SP5100 / SB800 / EFCH southbridge.

use anyhow::Result;
use serial_test::serial;
use tests_common::{require_root, skip_unless_identity, with_open};
use wdctl::{identity_str, options::*};

const IDENTITY: &str = "SP5100 TCO Watchdog";

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

// TODO: PCI scan correctness — verify the sp5100_tco-drv module loads
// only when the matching vendor:device tuple is on the bus, and the
// dmesg "[RUST] Scanning for SP5100 PCI devices..." line appears.
