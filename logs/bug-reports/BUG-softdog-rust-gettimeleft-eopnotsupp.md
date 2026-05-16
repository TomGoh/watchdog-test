# Bug：Rust 移植版 softdog-rust 未实现 WDIOC_GETTIMELEFT，返回 EOPNOTSUPP

**严重程度：** Medium（核心 ping/keepalive 工作，但 timeleft 缺失影响 watchdog 守护进程使用）
**发现日期：** 2026-05-15
**目标机：** N80（Kylin V11，aarch64，Phytium 平台）
**内核版本：** `Linux kylin 6.6.103+ #4 SMP Fri May 15 15:51:57 CST 2026 aarch64`
**Source Version：** `759db4212af54f42bf385066fdae69947f550748`
**模块：** `softdog-rust.ko`（位于 `/lib/modules/$(uname -r)/kernel/drivers/watchdog/`）

---

## 1. 摘要

`softdog-rust` 驱动加载后，`/dev/watchdog1`（softdog 实例）的大部分
ioctl 工作正常（GETSUPPORT/GETTIMEOUT/KEEPALIVE 都 OK），但
**`WDIOC_GETTIMELEFT` 返回 `EOPNOTSUPP`（"Operation not supported"）**。

这意味着 userspace 无法查询"watchdog 还有多少秒就要 fire"——这是
watchdog 守护进程（如 `watchdogd`、`systemd-watchdog`）做自适应
ping 间隔计算的关键 ioctl，缺失会让它们要么用默认间隔要么报错。

## 2. 复现步骤

```bash
sudo modprobe softdog-rust nowayout=0
sleep 1
# softdog 通常是 watchdog1 当系统也加载了 sbsa_gwdt-rust 时；
# 也可以直接 grep identity 找：
WD=$(for w in /sys/class/watchdog/watchdog*; do
       [ "$(cat $w/identity)" = "Software Watchdog (Rust)" ] && echo /dev/$(basename $w)
     done)
echo "softdog at $WD"

sudo python3 - <<PY
import fcntl, struct, os
fd = os.open("$WD", os.O_RDWR)

# WDIOC_GETSUPPORT — works
buf = bytearray(40)
fcntl.ioctl(fd, 0x80285700, buf)
print("GETSUPPORT OK, options=0x%x" % struct.unpack("<I", buf[:4])[0])

# WDIOC_GETTIMEOUT — works
fcntl.ioctl(fd, 0x80045707, struct.pack("i", 0))
print("GETTIMEOUT OK")

# WDIOC_KEEPALIVE — works
fcntl.ioctl(fd, 0x80045705, struct.pack("i", 0))
print("KEEPALIVE OK")

# WDIOC_GETTIMELEFT — FAILS
try:
    fcntl.ioctl(fd, 0x8004570a, struct.pack("i", 0))
    print("GETTIMELEFT OK")
except OSError as e:
    print("GETTIMELEFT FAIL:", e)

os.write(fd, b"V")
os.close(fd)
PY
```

**实际输出：**
```
GETSUPPORT OK, options=0x8180
GETTIMEOUT OK
KEEPALIVE OK
GETTIMELEFT FAIL: [Errno 95] Operation not supported
```

## 3. 受影响的 ioctl

| ioctl | softdog-rust /dev/watchdog1 |
|---|---|
| WDIOC_GETSUPPORT | ✅ OK |
| WDIOC_GETTIMEOUT | ✅ OK |
| WDIOC_KEEPALIVE | ✅ OK |
| `write(fd, "x", 1)` | ✅ OK |
| **WDIOC_GETTIMELEFT** | ❌ **EOPNOTSUPP** |

## 4. 根因

`drivers/watchdog/watchdog_dev.c:watchdog_ioctl` 处理 `WDIOC_GETTIMELEFT`
的代码：

```c
case WDIOC_GETTIMELEFT:
    if (!wdd->ops->get_timeleft) {
        err = -EOPNOTSUPP;
        break;
    }
    val = wdd->ops->get_timeleft(wdd);
    if (put_user(val, p))
        err = -EFAULT;
    break;
```

`-EOPNOTSUPP` 只来自 `wdd->ops->get_timeleft == NULL`。
所以 **Rust 端的 `WatchdogOps` 没填 `get_timeleft` 字段**。

需要内核同事在 `drivers/rust/drivers/watchdog/src/softdog.rs` 的
`SOFTDOG_OPS` 静态变量里加上 `get_timeleft: Some(softdog_get_timeleft)`，
对应的实现可以参考 in-tree C 版本 `softdog.c`：

```c
static unsigned int softdog_get_timeleft(struct watchdog_device *w)
{
    ktime_t remaining = hrtimer_get_remaining(&softdog_ticktock);
    return ktime_divns(remaining, NSEC_PER_SEC);
}
```

Rust 等价物大概形如：

```rust
unsafe extern "C" fn softdog_get_timeleft(_w: *mut WatchdogDevice) -> c_uint {
    let tt = unsafe { softdog_get_ticktock() };
    let remaining_ns = unsafe { hrtimer_get_remaining_ns(tt) };
    (remaining_ns / 1_000_000_000) as c_uint
}
```

并把它加到 `SOFTDOG_OPS` 里。

## 5. 测试套件中受影响的测试

```
common_conformance:
  c09_timeleft_in_range                 FAILED (EOPNOTSUPP)

common_extended:
  c_ext_01_write_byte_ping              FAILED (write OK, 但后续 timeleft 失败)
  c_ext_05_timeleft_progresses          FAILED (EOPNOTSUPP)

softdog (per-driver):
  softdog_ext_01_continuous_feed        FAILED (每次 ping 后查 timeleft 失败)
  softdog_ext_02_write_byte_ping        FAILED (write OK, timeleft 失败)
  softdog_ext_05_close_without_v_keeps_running  FAILED (查 timeleft 验证 timer 没被 reset)
```

## 6. 优先级建议

`get_timeleft` 不是必须实现的 watchdog op（kernel 接受它为 NULL），
所以严格说不是"违反 uapi"。但实际上：

- 现代的 watchdog 守护进程（systemd-watchdog 等）会读 timeleft 来
  决定 ping 间隔——缺了它要么用保守的固定间隔，要么报错；
- in-tree 的 C 版 `softdog` 是实现了 get_timeleft 的，**Rust 移植
  应该保持等价行为**。

所以建议在下一次构建里补上。

## 7. 完整日志

- `logs/2026-05-15-N80-autonomous-1617/tests.log`
- `logs/n80-retest2/live-journalctl.log`
