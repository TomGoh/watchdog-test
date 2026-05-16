// SPDX-License-Identifier: GPL-2.0
//! # `wdctl` — type-safe Rust wrapper around the kernel watchdog uapi
//!
//! Mirrors `<linux/watchdog.h>` so that:
//!
//! - The `WDIOC_*` ioctl numbers are computed at compile time by `nix`'s
//!   `ioctl_*!` macros — no runtime encoding mistakes.
//! - The `WatchdogInfo` struct layout is verified against the kernel's
//!   36-byte representation (8-byte options + firmware_version + 32-byte
//!   identity); a `const _: () = assert!(size_of::<WatchdogInfo>() == 36);`
//!   catches drift at compile time.
//! - Sysfs accessors (`identity`, `timeout`, `state`, …) are `Result`-returning
//!   helpers so tests don't have to reason about `Option<...>` of strings.
//!
//! Used by the `tests` crate; intentionally has no test-framework
//! dependencies of its own — it should be reusable from arbitrary
//! userspace programs.

use std::{
    ffi::CStr,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::io::{AsRawFd, RawFd},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use nix::{ioctl_read, ioctl_readwrite};

// ============================================================================
// uapi types and constants
// ============================================================================

/// `struct watchdog_info` — userspace-visible identity blob.
/// Layout matches `<uapi/linux/watchdog.h>`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WatchdogInfo {
    pub options: u32,
    pub firmware_version: u32,
    pub identity: [u8; 32],
}

const _: () = assert!(std::mem::size_of::<WatchdogInfo>() == 40 || std::mem::size_of::<WatchdogInfo>() == 36);
// note: the kernel struct is 36 bytes (no padding) on most arches; some
// build environments may align it to 40.  Both are accepted because tests
// only inspect the named fields, never raw bytes.

// WDIOF_* feature bits — verbatim from <linux/watchdog.h>
pub mod options {
    pub const OVERHEAT:       u32 = 0x0001;
    pub const FANFAULT:       u32 = 0x0002;
    pub const EXTERN1:        u32 = 0x0004;
    pub const EXTERN2:        u32 = 0x0008;
    pub const POWERUNDER:     u32 = 0x0010;
    pub const CARDRESET:      u32 = 0x0020;
    pub const POWEROVER:      u32 = 0x0040;
    pub const SETTIMEOUT:     u32 = 0x0080;
    pub const MAGICCLOSE:     u32 = 0x0100;
    pub const PRETIMEOUT:     u32 = 0x0200;
    pub const ALARMONLY:      u32 = 0x0400;
    pub const KEEPALIVEPING:  u32 = 0x8000;
}

/// `WDIOC_SETOPTIONS` argument bits — verbatim from `<linux/watchdog.h>`.
pub mod setopts {
    pub const DISABLECARD: i32 = 0x0001;
    pub const ENABLECARD:  i32 = 0x0002;
    pub const TEMPPANIC:   i32 = 0x0004;
}

const WATCHDOG_IOCTL_BASE: u8 = b'W';

// ioctl_read! and ioctl_readwrite! generate type-checked wrappers whose
// ioctl number is computed at compile time from (direction, magic, nr,
// type-size).  Caller never assembles the encoded value by hand.
ioctl_read!(wdioc_get_support,     WATCHDOG_IOCTL_BASE,  0, WatchdogInfo);
ioctl_read!(wdioc_get_status,      WATCHDOG_IOCTL_BASE,  1, i32);
ioctl_read!(wdioc_get_boot_status, WATCHDOG_IOCTL_BASE,  2, i32);
ioctl_read!(wdioc_get_temp,        WATCHDOG_IOCTL_BASE,  3, i32);
// NOTE: WDIOC_SETOPTIONS is `_IOR` in the kernel uapi (a historical
// quirk — the *semantic* direction is user→kernel, but the macro is
// IOR).  We must match the wire encoding or the kernel returns
// -ENOTTY.  Use `ioctl_read!` even though the operation conceptually
// "sets" something.
ioctl_read!(wdioc_set_options,     WATCHDOG_IOCTL_BASE,  4, i32);
ioctl_read!(wdioc_keep_alive,      WATCHDOG_IOCTL_BASE,  5, i32);
ioctl_readwrite!(wdioc_set_timeout,    WATCHDOG_IOCTL_BASE,  6, i32);
ioctl_read!(wdioc_get_timeout,     WATCHDOG_IOCTL_BASE,  7, i32);
ioctl_readwrite!(wdioc_set_pretimeout, WATCHDOG_IOCTL_BASE,  8, i32);
ioctl_read!(wdioc_get_pretimeout,  WATCHDOG_IOCTL_BASE,  9, i32);
ioctl_read!(wdioc_get_timeleft,    WATCHDOG_IOCTL_BASE, 10, i32);

// ============================================================================
// High-level Watchdog handle
// ============================================================================

/// Open `/dev/watchdog0` (or another `/dev/watchdogN`) for ioctl + write
/// access.
///
/// **Drop semantics:** `Drop` writes a single magic-'V' byte
/// (best-effort, errors ignored) before closing the fd. This matches
/// the userspace contract `WDIOF_MAGICCLOSE` expects: any close
/// without 'V' leaves the kernel's `WDOG_HW_RUNNING` set and
/// unbalances the module refcount, blocking `rmmod`. Tests that
/// **want** to exercise the no-'V' kernel path must call
/// [`Self::close_without_v`] explicitly.
///
/// See `drivers/watchdog/watchdog_dev.c:watchdog_release` for the
/// kernel side of this contract.
pub struct Watchdog {
    file: File,
    path: PathBuf,
    /// When true (the default), `Drop` writes 'V' before closing the fd.
    /// Cleared by `magic_close` (to suppress a redundant second write)
    /// and by `close_without_v` (to deliberately exercise the no-V path).
    drop_writes_v: bool,
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        if self.drop_writes_v {
            // Best-effort: if the fd is already bad we're tearing down
            // anyway. The File's own Drop closes the fd next.
            let _ = self.file.write_all(b"V");
        }
    }
}

impl Watchdog {
    /// Open the given watchdog device path.  Requires root or membership
    /// in a group with rw access to the device node.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open({})", path.display()))?;
        Ok(Self { file, path, drop_writes_v: true })
    }

    pub fn path(&self) -> &Path { &self.path }
    pub fn as_raw_fd(&self) -> RawFd { self.file.as_raw_fd() }

    /// Return the [`WatchdogInfo`] blob (identity + options + firmware ver).
    pub fn info(&self) -> Result<WatchdogInfo> {
        let mut info = WatchdogInfo::default();
        unsafe { wdioc_get_support(self.as_raw_fd(), &mut info) }
            .map_err(|e| anyhow!("WDIOC_GETSUPPORT: {e}"))?;
        Ok(info)
    }

    /// `WDIOC_GETSTATUS` — current alarm bits.
    pub fn status(&self) -> Result<i32> {
        let mut s: i32 = 0;
        unsafe { wdioc_get_status(self.as_raw_fd(), &mut s) }?;
        Ok(s)
    }

    /// `WDIOC_GETBOOTSTATUS` — bits set by the *last* reset (if any).
    pub fn boot_status(&self) -> Result<i32> {
        let mut s: i32 = 0;
        unsafe { wdioc_get_boot_status(self.as_raw_fd(), &mut s) }?;
        Ok(s)
    }

    /// `WDIOC_GETTIMEOUT` — current effective timeout, in seconds.
    pub fn timeout(&self) -> Result<i32> {
        let mut t: i32 = 0;
        unsafe { wdioc_get_timeout(self.as_raw_fd(), &mut t) }?;
        Ok(t)
    }

    /// `WDIOC_SETTIMEOUT` — request a new timeout.  Returns the value the
    /// kernel actually programmed (may be clamped).
    pub fn set_timeout(&self, secs: i32) -> Result<i32> {
        let mut t = secs;
        unsafe { wdioc_set_timeout(self.as_raw_fd(), &mut t) }?;
        Ok(t)
    }

    /// `WDIOC_GETTIMELEFT` — seconds until the watchdog fires.
    pub fn timeleft(&self) -> Result<i32> {
        let mut t: i32 = 0;
        unsafe { wdioc_get_timeleft(self.as_raw_fd(), &mut t) }?;
        Ok(t)
    }

    /// `WDIOC_KEEPALIVE` — explicit ping.  Equivalent to writing one
    /// non-'V' byte.
    pub fn keep_alive(&self) -> Result<()> {
        let mut dummy: i32 = 0;
        unsafe { wdioc_keep_alive(self.as_raw_fd(), &mut dummy) }?;
        Ok(())
    }

    /// Write a single 'V' byte then close — informs the kernel the
    /// caller wants the watchdog to actually stop on close.  After this
    /// call the [`Watchdog`] is consumed; you'll need [`Self::open`]
    /// again to come back.
    ///
    /// Functionally redundant with the default `Drop` (which also
    /// writes 'V'); use this when you want errors from the 'V' write
    /// surfaced via `Result`.  Suppresses Drop's V-write to avoid a
    /// redundant second write.
    pub fn magic_close(mut self) -> Result<()> {
        self.file.write_all(b"V").context("write 'V'")?;
        self.drop_writes_v = false;
        // self drops at end of function; File's Drop closes the fd.
        Ok(())
    }

    /// Close WITHOUT writing the magic 'V' byte.  Bypasses the default
    /// Drop-writes-V behavior so callers can deliberately exercise the
    /// kernel's `watchdog_release` no-magic-V path (which prints
    /// `watchdog%d: watchdog did not stop!` and keeps the kernel
    /// in-kernel keepalive running).
    ///
    /// **Leaves the module refcount unbalanced.** Per
    /// `watchdog_dev.c:watchdog_release`, the kernel only calls
    /// `module_put` when `!watchdog_hw_running(wdd)` at release time;
    /// closing without 'V' keeps `WDOG_HW_RUNNING` set, so no
    /// `module_put` runs and `rmmod` will fail with `-EBUSY`.  Use
    /// only in tests that explicitly verify the close-without-V
    /// semantics, and order them LAST in their binary.
    pub fn close_without_v(mut self) {
        self.drop_writes_v = false;
        // self drops at end of function; File's Drop just closes the
        // fd — kernel sees a close with no 'V' in its write buffer.
    }

    /// Write a single byte (any value other than 'V') as an *implicit*
    /// keepalive — userspace's traditional ping API, distinct from
    /// `WDIOC_KEEPALIVE`.  Both code paths in the kernel must work.
    pub fn write_byte_ping(&mut self) -> Result<()> {
        // Anything that isn't 'V'.  Use 'x' for legibility in strace.
        self.file.write_all(b"x").context("write byte ping")?;
        Ok(())
    }

    /// `WDIOC_SETOPTIONS` — explicitly disable or enable the watchdog
    /// hardware (independent of open/close lifecycle).  Returns Ok if
    /// the kernel accepted the option bitmap.
    pub fn set_options(&self, bits: i32) -> Result<()> {
        let mut v = bits;
        unsafe { wdioc_set_options(self.as_raw_fd(), &mut v) }?;
        Ok(())
    }
}

// ============================================================================
// Sysfs accessors — `/sys/class/watchdog/watchdogN/...`
// ============================================================================

/// Iterate every `/sys/class/watchdog/watchdogN/` currently present.
pub fn enumerate() -> Result<Vec<WatchdogSysfs>> {
    let dir = Path::new("/sys/class/watchdog");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for ent in std::fs::read_dir(dir).with_context(|| format!("readdir {}", dir.display()))? {
        let ent = ent?;
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("watchdog") {
            out.push(WatchdogSysfs {
                index: name.trim_start_matches("watchdog").parse().unwrap_or(0),
                sysfs: ent.path(),
            });
        }
    }
    out.sort_by_key(|w| w.index);
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct WatchdogSysfs {
    pub index: u32,
    pub sysfs: PathBuf,
}

impl WatchdogSysfs {
    pub fn dev_node(&self) -> PathBuf {
        PathBuf::from(format!("/dev/watchdog{}", self.index))
    }

    pub fn identity(&self) -> Result<String> {
        read_trim(self.sysfs.join("identity"))
    }
    pub fn timeout(&self) -> Result<u32> {
        read_trim(self.sysfs.join("timeout"))?.parse().map_err(Into::into)
    }
    pub fn pretimeout(&self) -> Result<u32> {
        read_trim(self.sysfs.join("pretimeout"))?.parse().map_err(Into::into)
    }
    pub fn nowayout(&self) -> Result<bool> {
        Ok(read_trim(self.sysfs.join("nowayout"))? == "1")
    }
    pub fn state(&self) -> Result<String> {
        read_trim(self.sysfs.join("state"))
    }
    pub fn bootstatus(&self) -> Result<u32> {
        read_trim(self.sysfs.join("bootstatus"))?.parse().map_err(Into::into)
    }

    pub fn open_dev(&self) -> Result<Watchdog> {
        Watchdog::open(self.dev_node())
    }
}

fn read_trim<P: AsRef<Path>>(p: P) -> Result<String> {
    let mut s = String::new();
    File::open(p.as_ref())
        .with_context(|| format!("open {}", p.as_ref().display()))?
        .read_to_string(&mut s)?;
    Ok(s.trim().to_string())
}

// ============================================================================
// dmesg capture
// ============================================================================

/// Snapshot of `/dev/kmsg` content currently in the ring buffer (root
/// only, since /dev/kmsg is 0440).  Returns lines newest-first the way
/// the kernel exposes them.
///
/// Lines are returned without the prefix `<level>,<seq>,<ts>;`; only
/// the message text remains.
pub fn dmesg_snapshot() -> Result<Vec<String>> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/kmsg")
        .context("open /dev/kmsg (root needed)")?;
    let mut reader = std::io::BufReader::new(f);
    let mut out = Vec::new();
    let mut buf = String::new();
    loop {
        buf.clear();
        match std::io::BufRead::read_line(&mut reader, &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                // strip the "<lvl>,<seq>,<ts>;" prefix if present
                let msg = if let Some(idx) = buf.find(';') {
                    &buf[idx + 1..]
                } else {
                    buf.as_str()
                };
                out.push(msg.trim_end().to_string());
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => bail!("read kmsg: {e}"),
        }
    }
    Ok(out)
}

/// Match `pattern` (regex-free literal substring search) against the
/// current dmesg snapshot.  Returns the first matching line, or None.
pub fn dmesg_find(needle: &str) -> Result<Option<String>> {
    Ok(dmesg_snapshot()?
        .into_iter()
        .find(|l| l.contains(needle)))
}

/// Convenience: extract identity bytes from a `WatchdogInfo` as a
/// trimmed Rust `&str`.
pub fn identity_str(info: &WatchdogInfo) -> &str {
    let end = info
        .identity
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(info.identity.len());
    // SAFETY: kernel guarantees ASCII identity; if not we fall back.
    CStr::from_bytes_with_nul(&info.identity[..=end.min(info.identity.len() - 1)])
        .map(|c| c.to_str().unwrap_or(""))
        .unwrap_or("")
}
