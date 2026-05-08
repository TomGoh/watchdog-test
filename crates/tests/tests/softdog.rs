// SPDX-License-Identifier: GPL-2.0
//! softdog-specific tests (placeholder).  Auto-skip if the running
//! driver isn't softdog.

use anyhow::Result;
use serial_test::serial;
use tests_common::{require_root, skip_unless_identity, with_open};
use wdctl::{identity_str, options::*};

const IDENTITY: &str = "Software Watchdog";

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

#[test]
#[serial(watchdog)]
fn softdog_02_keepalive_advertised() -> Result<()> {
    require_root()?;
    let Some(sys) = skip_unless_identity(IDENTITY)? else {
        return Ok(());
    };
    with_open(&sys, |w| {
        let info = w.info()?;
        assert!(info.options & KEEPALIVEPING != 0);
        assert!(info.options & MAGICCLOSE != 0);
        Ok(())
    })
}

// TODO: pretimeout-bound tests once we wire up CONFIG_SOFT_WATCHDOG_PRETIMEOUT
// awareness; module_param-based behaviour tests (soft_panic / soft_noboot)
// once the test runner supports rmmod/modprobe with options.
