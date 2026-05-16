# Bug：Rust 移植版 sbsa_gwdt-rust 所有 watchdog ioctl 都返回 ENOTTY

**严重程度：** Critical（驱动基本功能完全不可用）
**发现日期：** 2026-05-15
**目标机：** N80（Kylin V11，aarch64，Phytium 平台，8 核）
**内核版本：** `Linux kylin 6.6.103+ #4 SMP Fri May 15 15:51:57 CST 2026 aarch64`
**Source Version：** `759db4212af54f42bf385066fdae69947f550748`
**模块：** `sbsa_gwdt-rust.ko`（位于 `/lib/modules/$(uname -r)/kernel/drivers/watchdog/`）

---

## 1. 摘要

加载 `sbsa_gwdt-rust` 后，`/dev/watchdog0` 创建成功、sysfs 节点正常，
但**任何 `ioctl()` 调用都返回 `ENOTTY`（"Inappropriate ioctl for device"）**。
这意味着 watchdog 框架接口层完全不通——userspace 拿不到 identity、
拿不到 options、不能 set timeout、不能 keepalive。

`write(fd, "x", 1)` 路径也间接受影响，因为 `watchdog_write` 内部
会路由到 `wdd->ops` 上对应的回调。

## 2. 复现步骤

```bash
# 内核要求：CONFIG_WATCHDOG_NOWAYOUT=n（无要求 nowayout 也可，仅影响清理）
sudo modprobe sbsa_gwdt-rust nowayout=0
sleep 1
ls /sys/class/watchdog/    # 期望：watchdog0 存在
cat /sys/class/watchdog/watchdog0/identity
# → "SBSA Generic Watchdog"  ✓

sudo python3 - <<'PY'
import fcntl, struct, os
fd = os.open("/dev/watchdog0", os.O_RDWR)
# WDIOC_GETSUPPORT = _IOR('W', 0, 40)
buf = bytearray(40)
try:
    fcntl.ioctl(fd, 0x80285700, buf)
    print("GETSUPPORT OK")
except OSError as e:
    print("GETSUPPORT FAIL:", e)
# WDIOC_KEEPALIVE = _IOR('W', 5, int)
try:
    fcntl.ioctl(fd, 0x80045705, struct.pack("i", 0))
    print("KEEPALIVE OK")
except OSError as e:
    print("KEEPALIVE FAIL:", e)
os.write(fd, b"V")
os.close(fd)
PY
```

**实际输出：**
```
GETSUPPORT FAIL: [Errno 25] Inappropriate ioctl for device
KEEPALIVE FAIL: [Errno 25] Inappropriate ioctl for device
```

**期望输出：**
```
GETSUPPORT OK
KEEPALIVE OK
```

## 3. 受影响的 ioctl 完整列表

实测在 `/dev/watchdog0`（sbsa_gwdt-rust）上的结果：

| ioctl | 返回 |
|---|---|
| WDIOC_GETSUPPORT | ENOTTY |
| WDIOC_GETTIMEOUT | ENOTTY |
| WDIOC_GETTIMELEFT | ENOTTY |
| WDIOC_KEEPALIVE | ENOTTY |
| WDIOC_SETTIMEOUT | ENOTTY |
| WDIOC_SETOPTIONS | ENOTTY |
| WDIOC_GETBOOTSTATUS | ENOTTY |
| WDIOC_GETSTATUS | ENOTTY |

**没有一个 ioctl 工作。**`write()` 调用本身有时能成功（写一个非 'V'
字节作为 ping 也走 `wdd->ops->ping`），但通过 ioctl 触发的等价操作
全部失败。

## 4. 测试套件中受影响的测试

来自 `watchdog-test` 套件 `2026-05-15-N80-autonomous-1617` 这次跑：

```
common_conformance:
  c01_sysfs_entry_exists                FAILED
  c02_identity_consistent               FAILED (WDIOC_GETSUPPORT: ENOTTY)
  c03_options_advertise_keepaliveping   FAILED (WDIOC_GETSUPPORT: ENOTTY)
  c05_keepalive_ioctl_works             FAILED (ENOTTY)
  c07_set_timeout_round_trip            FAILED
  c08_set_timeout_zero_rejected         FAILED (WDIOC_GETSUPPORT: ENOTTY)
  c09_timeleft_in_range                 FAILED (WDIOC_GETSUPPORT: ENOTTY)

common_extended:
  c_ext_01_write_byte_ping              FAILED
  c_ext_02_concurrent_open_ebusy        FAILED (WDIOC_GETSUPPORT: ENOTTY)
  c_ext_03_set_timeout_clamps_oversize  FAILED (second open succeeded — exclusivity broken?)
  c_ext_04_bootstatus_readable          FAILED (WDIOC_GETSUPPORT: ENOTTY)
  c_ext_05_timeleft_progresses          FAILED (ENOTTY)

sbsa_gwdt:
  sbsa_01_identity                      FAILED
  sbsa_02_options_bitmap                FAILED (WDIOC_GETSUPPORT: ENOTTY)
  sbsa_ext_01_continuous_feed           FAILED (ENOTTY)
  sbsa_ext_02_write_byte_ping           FAILED
  sbsa_ext_03_settimeout_matrix         所有 SETTIMEOUT 全部被拒绝（1s/5s/10s/30s/60s/80s）
  sbsa_ext_04_setoptions_disable_enable FAILED
  sbsa_ext_05_close_without_v_keeps_running FAILED (ENOTTY)
```

## 5. 怀疑方向

`ENOTTY` 来自 watchdog_dev 框架——当它发现传进来的 ioctl 编号没有
匹配的 case，或者发现 `wdd->ops` 上对应的回调指针为 NULL 时，会返回
ENOTTY。看 `drivers/watchdog/watchdog_dev.c:watchdog_ioctl` 的逻辑：

```c
case WDIOC_GETSUPPORT:
    if (copy_to_user(...)) ...
    break;
```

`WDIOC_GETSUPPORT` 直接 copy `wdd->info` 给 userspace，不依赖 ops。
如果它都 ENOTTY，**说明 ioctl dispatch 在 watchdog_ioctl 入口之前
就被截胡了**——很可能是：

1. **`wdd->info` 是 NULL** 或者 `info` 字段没设置——不过这种情况
   通常会 OOPS 而不是 ENOTTY；
2. 或者 **fops 没正确注册到 misc device**——但既然 `/dev/watchdog0`
   存在能 open，那 cdev 是注册了的；
3. 或者 **watchdog_register_device 返回了错误但模块没正确处理**
   ——device 注册不完整，但又留下了 sysfs 残骸；
4. 或者 **Rust 端给 `wdd->ops` 赋的指针是 NULL**——但这通常会触发
   "no_ioctl" return EOPNOTSUPP 而不是 ENOTTY。

最可能的怀疑：**Rust 移植版 `sbsa_gwdt-rust` 的 `register_device`
路径走错了，注册了一个没绑 ops 的"半截 watchdog_device"**——sysfs
能 ls，但 `watchdog_dev` 拿到 ioctl 找不到 `wdd->ops`，dispatch 失败
返回 ENOTTY。

需要内核同事看：
- `drivers/rust/drivers/watchdog/src/sbsa_gwdt.rs` 的 `register_device`
  调用之前 `wdd->info` 和 `wdd->ops` 是否都已经被赋了非 NULL 指针；
- 把 `WatchdogOps` 结构体打印出来看每个字段是不是函数指针。

## 6. 历史背景

上一次跑测试（kernel `6.6.103+ #2`，`sbsa_gwdt-drv.ko`，未改名前），
同样的 SBSA 驱动这些 ioctl 是工作的——日志显示 `c05_keepalive_ioctl_works`
通过、`sbsa_ext_03_settimeout_matrix` 各种 timeout 也能 round-trip。
所以这是 **`#2 → #4` 两次构建之间出现的回归**。可以对比 git diff：

```bash
git -C ~/klinux log --oneline drivers/rust/drivers/watchdog/src/sbsa_gwdt.rs
git -C ~/klinux log --oneline drivers/watchdog/sbsa_gwdt.c
```

## 7. 完整日志

- `logs/2026-05-15-N80-autonomous-1617/tests.log`
- `logs/2026-05-15-N80-autonomous-1617/dmesg-pre.log` / `dmesg-post.log` / `dmesg-delta.log`
- `logs/n80-retest2/live-journalctl.log`
