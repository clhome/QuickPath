# QuickPath CI 失败修复记录(`test_clipboard_busy_is_reported_and_never_claims_success`)

**触发**:GitHub Actions 在最新提交上运行 `cargo test --all-targets` 报错:

```
---- win32::clipboard::tests::test_clipboard_busy_is_reported_and_never_claims_success stdout ----
thread '...' panicked at src\win32\clipboard.rs:2862:9:
assertion failed: set_raw_clipboard_text("qp_busy_seed")
test result: FAILED. 227 passed; 1 failed; 0 ignored ...
```

**关键观察**:失败点是**夹具的第一行**(写种子剪贴板),**不是任何业务断言**;
其余 227 个用例全部通过。本地(12 核)并行与串行均**无法复现**。

---

## 1. 排查过程(先取证,不猜)

按顺序排除,每步都基于代码事实:

1. **失败位置**是 `assert!(set_raw_clipboard_text("qp_busy_seed"))`,即 `clipboard_atomic_write` 未成功。
   `set_raw_clipboard_text_tracked` 的失败只可能来自:①`expected_sequence == 0`(OwnershipUnknown)、
   ②`current != expected`(NotOwned)、③剪贴板打不开(Busy)、④某个格式写入失败。
2. **测试锁**:原判断是"并行下漏锁"。写脚本用**花括号配平**精确提取每个用例函数体后确认:
   **所有写种子的用例都持有 `clip_test_lock`**,锁不是问题。
3. **全局注入开关**:扫描 `TEST_FORCE_BUSY` / `TEST_FORCE_SEQ_ZERO` / `TEST_FORCE_NO_OWNER` /
   `TEST_FAIL_FORMAT` / `TEST_OWNER_HOOKS` / `TEST_BEFORE_ATOMIC_WRITE` 的置位与复位点。
   发现 `arm_atomic_write_hook` 是**进程级单槽静态量**,其闭包会把 `TEST_FAIL_FORMAT` 置位。
4. **加诊断(关键一步)**:把夹具失败原因打印出来(原本失败**不带任何信息**)。
   重跑后立刻拿到真因:

   ```
   [clipboard test fixture] 播种剪贴板连续 6 次失败(最后一次原因: 部分格式写入失败: [13])
   ```

   `13` = `CF_UNICODETEXT` ⇒ **`TEST_FAIL_FORMAT` 被置位且没有还原**。
5. **定位泄漏源**:`run_atomic_write_hook` 的 `skip` 只在**原子写入被调用时**递减。
   用例 `test_temporary_clipboard_text_propagates_restore_failure` 用 `skip = 1`(需要 ≥2 次原子写入);
   一旦实际调用次数不足,或用例在钩子被消费前就失败/提前返回,**钩子就留在静态量里**,
   由**后续某个无关用例**的写入去触发 —— 于是那个无关用例"在夹具第一步莫名其妙失败"。

---

## 2. 根因

**测试旋钮(`TEST_BEFORE_ATOMIC_WRITE` 单槽钩子)没有 RAII 清理机制。**

- 安装没有返回 guard,用例结束(正常或异常)都不会撤销未消费的钩子;
- 钩子残留后会修改**进程级全局开关** `TEST_FAIL_FORMAT` / `TEST_FAIL_MODE`,且**无人还原**;
- 后果是**跨用例污染**:失败表象出现在无关用例上,定位代价极高 ——
  这也解释了为什么"本地复现不了、CI 偶发":能否复现取决于用例执行顺序与钩子是否恰好被消费。

这与项目里已经总结过的教训完全同源:**任何 arm/disable 型测试旋钮都必须配 RAII guard**
(前一轮 `test_gate` 正是因为缺 guard 导致断言失败时整个 `cargo test` 挂死,已加 `GateGuard`)。
本轮是同一类缺陷的第二个实例。

---

## 3. 修复(`src/win32/clipboard.rs`,单文件)

1. **`arm_atomic_write_hook` 返回 RAII guard**:
   ```rust
   fn arm_atomic_write_hook(skip: usize, action: ...) -> ArmHookGuard
   ```
   `impl Drop for ArmHookGuard` → `disarm_atomic_write_hook()`,
   **无论正常结束、断言失败还是 panic 展开都会撤销**残留钩子。
2. **安装时断言无残留**(`guard.is_none()`,纵深防御):真发生泄漏时,失败点落在
   **泄漏源头**而不是后面的无关用例,避免再次"隔山打牛"。
3. **5 处调用点全部绑定 guard**(`let _hook = ...`);并明确注释:
   禁止写 `let _ = arm_atomic_write_hook(..)`(guard 会立即 drop、旋钮直接撤销)。
4. **夹具失败打印原因**:`set_raw_clipboard_text_tracked` 失败时 `eprintln!` 具体原因,
   并在文本里直接提示「含 13 即疑似注入开关泄漏」。原实现失败时**不带任何信息**,
   是这次定位成本高的直接原因。
5. **新增 CI 稳定性守卫** `test_clipboard_test_knobs_are_raii_cleaned`,钉住四条:
   安装器必须返回 guard、guard 的 `Drop` 必须真的撤销、安装时必须断言无残留、
   调用点不得用 `let _ =` 静默丢弃 guard。
6. 同类单槽旋钮 `block_owner_pump_for_test` 已核查(两处调用点均在用例内显式释放),
   本轮只加注释说明其属同一类别,未改动行为。

**未放宽任何业务断言**:所有 `assert!` 与 fail-closed 语义保持原样;
本次只修测试基础设施的清理与诊断。

---

## 4. 门禁(本机真实执行)

| 项 | 结果 |
|---|---|
| `cargo fmt --check` | 干净 |
| `cargo clippy --all-targets -- -D warnings` | 0 告警 |
| `cargo test --all-targets` | **229 通过 0 失败**(228 → 229,+1 守卫) |
| `cargo test --all-targets` 默认并行连跑 3 次 | 每次 **229 通过 0 失败**(验证修复的稳定性) |
| `cargo test --all-targets -- --test-threads=1` | **229 通过 0 失败** |
| `cargo audit` | 退出码 0 |
| `cargo build --release` | 通过(1.2.3) |

---

## 5. 变异自证

| # | 变异 | 结果 |
|---|---|---|
| M1 | `ArmHookGuard::drop` 不再撤销(退回修复前行为) | ❌ **跨用例污染重现**:`test_totalcmd_delayed_two_notifications_rejects_claim` + `test_xyplorer_delayed_two_notifications_rejects_claim` 双双失败(正是 CI 的同类症状),证明修复真的消除了泄漏 |
| M2 | 去掉安装时的「无残留」断言 | ✅ 未变红 —— **属预期**:M1 已证明 RAII guard 使残留不可能发生,该断言只是纵深防御。**不宣称 M2 可被测试钉住**,如实登记 |
| M3 | 调用点改为 `let _ = arm_atomic_write_hook(..)` | ❌ 守卫 `test_clipboard_test_knobs_are_raii_cleaned` + 行为用例 `test_write_owned_text_rechecks_...` 双双变红 |

M2 的诚实边界:M1 的存在使残留不可能发生,M2 的断言无法被"损坏产生"来验证;
它属于防御性代码,由 M1/M3 的组合覆盖间接保证。若后续有人移除 RAII guard 且同时移除该断言,
M1 类的变异仍然会红(行为层),因此安全网未失效。

---

## 6. 未闭合项

1. **CI 侧复跑确认**:本轮只完成本机验证;**需在 GitHub Actions 上复跑同一提交确认转绿**
   (沿用 CI 证据未闭合项)。
2. 真机验收清单 §10~§15 全部未闭合项不变。
3. 同类风险提示:仓库里其余"进程级单槽旋钮"若有新增,应同样配 RAII guard;
   本轮只加了 `arm_atomic_write_hook` 的守卫与 `block_owner_pump_for_test` 的注释核查,
   未做全仓强制门禁(避免过度工程)。