// SPDX-License-Identifier: GPL-2.0
//! gc_test — end-to-end watchdog driver verification suite covering the
//! four validation items required by the QA procedure:
//!
//!   1. Open device, inspect device info
//!      → `gc_01_device_info`
//!   2. Configure timeout, verify in-window ping keeps the box alive
//!      → `gc_02_feed_within_timeout`
//!   3. Without ping past the timeout, hardware reset fires
//!      → `gc_03_no_feed_reboot`  (`#[ignore]`, lab-gated — REBOOTS the box)
//!   4. With watchdog NOT armed, rmmod + modprobe 10 times in a row;
//!      device node recreates each iteration, ops re-bind, no leftover
//!      fd holder occupies the device
//!      → `gc_04_modprobe_cycle_x10`  (`#[ignore]`, slow ~30-60s)
//!
//! Driver-agnostic: targets whichever watchdog
//! [`tests_common::pick_watchdog`] resolves to.  Set
//! `WATCHDOG_TEST_IDENTITY="SBSA Generic Watchdog"` (etc.) to pin a
//! specific driver in a multi-watchdog target.
//!
//! Console output is in Chinese so the QA report reads naturally;
//! source-level comments stay in English for the engineering audience.
//!
//! Invocation examples on the target:
//!
//! ```bash
//! # All non-destructive items (gc_01 + gc_02 + gc_04):
//! WATCHDOG_TEST_IDENTITY="SBSA Generic Watchdog" \
//!   sudo -E /tmp/watchdog-test/gc_test-* --include-ignored --nocapture --test-threads=1
//!
//! # Just the destructive reset test (will reboot):
//! WATCHDOG_TEST_IDENTITY="SBSA Generic Watchdog" \
//!   WATCHDOG_LAB_DANGEROUS=YES_REALLY \
//!   sudo -E /tmp/watchdog-test/gc_test-* gc_03 --include-ignored --nocapture --test-threads=1
//! ```

use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serial_test::serial;
use tests_common::{describe, pick_watchdog, require_lab_consent, require_root, with_open};
use wdctl::{identity_str, options::*};

/// Map a watchdog identity to its kernel module name so gc_04 knows
/// what to rmmod/modprobe.  Keep in sync with scripts/deploy.sh.
fn module_for_identity(id: &str) -> Option<&'static str> {
    match id {
        "SBSA Generic Watchdog" => Some("sbsa_gwdt-rust"),
        "Software Watchdog (Rust)" => Some("softdog-rust"),
        "SP5100 TCO Watchdog" => Some("sp5100_tco-rust"),
        _ => None,
    }
}

fn modcmd(args: &[&str]) -> Result<()> {
    let out = Command::new(args[0]).args(&args[1..]).output()?;
    if !out.status.success() {
        bail!(
            "{args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

// ============================================================================
// GC-01  打开设备、检视设备信息正常
// ============================================================================
//
// Verifies:
//   - /sys/class/watchdog/<N>/ readable (identity, timeout, nowayout, state)
//   - /dev/watchdog<N> opens
//   - WDIOC_GETSUPPORT returns a non-empty identity string identical to
//     sysfs and a non-zero options bitmap that advertises KEEPALIVEPING
#[test]
#[serial(watchdog)]
fn gc_01_device_info() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    println!("# 测试设备：{}", describe(&sys)?);

    let sysfs_id = sys.identity()?;
    let sysfs_timeout = sys.timeout()?;
    let sysfs_nowayout = sys.nowayout()?;
    let sysfs_state = sys.state()?;
    println!(
        "# sysfs 信息：identity={sysfs_id:?} timeout={sysfs_timeout}s \
         nowayout={sysfs_nowayout} state={sysfs_state:?}"
    );
    assert!(!sysfs_id.is_empty(), "sysfs identity 为空");

    with_open(&sys, |w| {
        let info = w.info()?;
        let ioctl_id = identity_str(&info).to_string();
        println!(
            "# ioctl 信息：identity={ioctl_id:?} options=0x{:04x} firmware_version={}",
            info.options, info.firmware_version
        );
        assert_eq!(
            sysfs_id, ioctl_id,
            "sysfs 与 WDIOC_GETSUPPORT 拿到的 identity 不一致"
        );
        assert!(
            info.options != 0,
            "WDIOC_GETSUPPORT.options 为 0 —— 驱动没有正确填充 WatchdogInfo"
        );
        assert!(
            info.options & KEEPALIVEPING != 0,
            "驱动没有声明 KEEPALIVEPING 能力 (options=0x{:04x})",
            info.options
        );
        println!("# GC-01 通过：设备信息读取正常");
        Ok(())
    })
}

// ============================================================================
// GC-02  配置设备超时机制正常，设定的时间周期内喂狗正常
// ============================================================================
//
// Two phases:
//   (a) SETTIMEOUT round-trip at multiple values (skipped if driver
//       doesn't advertise WDIOF_SETTIMEOUT).
//   (b) Continuous keep_alive() ping every (timeout/2) seconds for
//       2× timeout — proves the feed mechanism keeps the box alive.
#[test]
#[serial(watchdog)]
fn gc_02_feed_within_timeout() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    println!("# 测试设备：{}", describe(&sys)?);

    with_open(&sys, |w| {
        let info = w.info()?;
        let original = sys.timeout()? as i32;

        // Phase (a): SETTIMEOUT round-trip.
        if info.options & SETTIMEOUT != 0 {
            println!("# 阶段 1：SETTIMEOUT 多值往返测试");
            for &want in &[5i32, 10, 30] {
                let actual = match w.set_timeout(want) {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "#   SETTIMEOUT({want}) 被拒绝：{e} \
                             （可能超过 max_hw_heartbeat）"
                        );
                        continue;
                    }
                };
                let echoed = sys.timeout()? as i32;
                assert_eq!(
                    actual, echoed,
                    "SETTIMEOUT({want}) 返回 {actual} 但 sysfs 读到 {echoed}"
                );
                println!("#   SETTIMEOUT({want}) → {actual}s ✓");
            }
            // Restore for the feed loop below.
            let _ = w.set_timeout(original);
        } else {
            println!("# 阶段 1：跳过 —— 驱动不支持 SETTIMEOUT");
        }

        // Phase (b): continuous feed for 2× timeout.
        let t = sys.timeout()? as u64;
        let half = (t / 2).max(1);
        let runtime = Duration::from_secs(t * 2);
        println!(
            "# 阶段 2：连续喂狗 {}s（timeout={}s，每 {}s ping 一次）",
            runtime.as_secs(),
            t,
            half
        );

        let start = Instant::now();
        let mut pings = 0u32;
        while start.elapsed() < runtime {
            w.keep_alive()?;
            pings += 1;
            std::thread::sleep(Duration::from_secs(half));
        }
        println!(
            "# GC-02 通过：累计喂狗 {pings} 次，{}s 内系统存活 —— 喂狗机制正常",
            runtime.as_secs()
        );
        Ok(())
    })
}

// ============================================================================
// GC-03  超时未喂狗则触发硬件复位  (DESTRUCTIVE — will reboot the box)
// ============================================================================
//
// Lab-gated by `WATCHDOG_LAB_DANGEROUS=YES_REALLY` (same consent gate
// as `lab_dangerous-*` tests).  Sets a 5 s arm, deliberately stops
// feeding, sleeps past the deadline.  If the test process returns,
// the driver failed to fire — that's a real driver bug.
#[test]
#[ignore = "lab-only: reboots the machine when working correctly"]
#[serial(watchdog)]
fn gc_03_no_feed_reboot() -> Result<()> {
    require_root()?;
    if !require_lab_consent()? {
        return Ok(());
    }
    let sys = pick_watchdog()?;
    println!("# 测试设备：{}", describe(&sys)?);

    let wdt = sys.open_dev()?;
    let info = wdt.info()?;
    let effective_arm: i32 = if info.options & SETTIMEOUT != 0 {
        let arm = 5;
        wdt.set_timeout(arm)?
    } else {
        let arm = sys.timeout()? as i32;
        println!("# 驱动不支持 SETTIMEOUT，沿用默认 timeout={arm}s");
        arm
    };
    let wait = Duration::from_secs((effective_arm as u64) + 5);
    println!(
        "# 已 arm timeout={effective_arm}s；预期 ~{}s 内硬件复位",
        wait.as_secs()
    );

    // Hold the fd open and do NOT ping.
    std::thread::sleep(wait);

    // If we wake up, the watchdog did not fire — driver bug.
    drop(wdt);
    bail!(
        "GC-03 失败：等待 {}s 后系统未复位 —— 驱动未触发硬件 reset",
        wait.as_secs()
    );
}

// ============================================================================
// GC-04  看门狗未使能状态下反复加载/卸载驱动 10 次
// ============================================================================
//
// For each iteration:
//   1. Verify the device with our identity is present in /sys/class/watchdog/.
//   2. `rmmod <module>` — if any fd holder remained, rmmod fails with
//      -EBUSY ("Module is in use"), which is the test's proof of "no
//      leftover processes occupying the device".
//   3. Poll until the sysfs entry disappears (up to 5 s).
//   4. `modprobe <module> nowayout=0`.
//   5. Poll until the new entry appears (up to 5 s).
//   6. Open it, send one keepalive ioctl, magic-V close — proves the
//      driver ops still wire up correctly after re-registration.
//
// Self-skips when `nowayout=1` (rmmod would unsync the in-kernel
// keepalive and reboot the box).
#[test]
#[ignore = "stress: 10 full rmmod+modprobe cycles, takes ~30-60s"]
#[serial(watchdog)]
fn gc_04_modprobe_cycle_x10() -> Result<()> {
    require_root()?;
    let sys = pick_watchdog()?;
    let identity = sys.identity()?;
    println!("# 测试设备：{}", describe(&sys)?);

    if sys.nowayout().unwrap_or(false) {
        println!(
            "# SKIP: nowayout=1 —— rmmod 会停止内核 keepalive 并导致复位/panic"
        );
        return Ok(());
    }

    let module = module_for_identity(&identity).ok_or_else(|| {
        anyhow!(
            "identity {identity:?} 不在 module_for_identity() 表里 —— \
             请同步更新 gc_test.rs 和 scripts/deploy.sh"
        )
    })?;
    println!(
        "# 对内核模块 {module:?}（identity={identity:?}）反复加载/卸载 10 次"
    );

    for i in 1..=10u32 {
        // Pre-condition: device present.
        let pre = wdctl::enumerate()?
            .into_iter()
            .find(|w| w.identity().ok().as_deref() == Some(identity.as_str()))
            .ok_or_else(|| {
                anyhow!(
                    "第 {i} 轮：rmmod 之前 identity={identity:?} 的设备节点不存在"
                )
            })?;
        let _ = pre.timeout()?; // sanity: sysfs reads work

        // rmmod will fail with -EBUSY if the module refcount is > 0.
        // Per watchdog_dev.c:watchdog_release, refcount stays > 0 when
        // a previous close left WDOG_HW_RUNNING set — which is what
        // happens for any close-without-'V' on a MAGICCLOSE driver.
        // wdctl::Watchdog::Drop writes 'V' by default precisely to
        // avoid this; if the test reaches here with a refcount leak,
        // it means something opened /dev/watchdog* and bypassed both
        // magic_close() and the V-writing Drop.
        if let Err(e) = modcmd(&["rmmod", module]) {
            let refs = std::fs::read_to_string(
                format!("/sys/module/{}/refcnt", module.replace('-', "_"))
            ).unwrap_or_else(|_| "?".to_string());
            bail!(
                "第 {i} 轮：rmmod {module} 失败：{e}\n\
                 /sys/module/{}/refcnt = {}\n\
                 → refcnt > 0 一般表示有路径 open 了设备但 close 时没写 'V'，\
                 让 kernel 把 WDOG_HW_RUNNING 留住、module_put 没跑成。\
                 检查测试代码里所有 open_dev() / Watchdog::open() 的调用，\
                 确保走 magic_close / 默认 Drop（写 V），\
                 或者外部有进程持着 /dev/watchdog*。",
                module.replace('-', "_"),
                refs.trim()
            )
        }

        // Poll until the device entry is gone (driver fully unregistered).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let gone = wdctl::enumerate()?
                .iter()
                .all(|w| w.identity().ok().as_deref() != Some(identity.as_str()));
            if gone {
                break;
            }
            if Instant::now() >= deadline {
                bail!(
                    "第 {i} 轮：rmmod 后 5s identity={identity:?} 的设备节点仍未消失 —— \
                     unregister 路径没有同步完成"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // Reload with explicit nowayout=0 to match the suite's contract.
        modcmd(&["modprobe", module, "nowayout=0"])
            .map_err(|e| anyhow!("第 {i} 轮：modprobe {module} nowayout=0 失败：{e}"))?;

        // Poll until the new entry appears.
        let deadline = Instant::now() + Duration::from_secs(5);
        let sys_after = loop {
            if let Some(s) = wdctl::enumerate()?
                .into_iter()
                .find(|w| w.identity().ok().as_deref() == Some(identity.as_str()))
            {
                break s;
            }
            if Instant::now() >= deadline {
                bail!(
                    "第 {i} 轮：modprobe 后 5s 内 identity={identity:?} 的设备节点没有重新出现"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        // Open + ping + magic-V close: prove ops work, then disarm cleanly.
        let wdt = sys_after.open_dev()?;
        wdt.keep_alive()?;
        wdt.magic_close()?;

        println!("# 第 {i:>2}/10 轮通过");
    }

    println!(
        "# GC-04 通过：10/10 轮完成 —— 无残留 fd 占用、identity 保持一致、\
         每次 reload 后 ops 仍可用"
    );
    Ok(())
}
