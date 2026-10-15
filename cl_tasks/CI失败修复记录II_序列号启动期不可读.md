# QuickPath CI 失败修复记录 II(序列号启动期不可读)

**触发**:GitHub Actions 在 `6506f93` 上 `cargo test --all-targets` 报:

```
---- win32::clipboard::tests::test_clipboard_busy_is_reported_and_never_claims_success stdout ----
[clipboard test fixture] 播种失败:OwnershipUnknown

panicked at src\win32\clipboard.rs:2914:9:
assertion failed: set_raw_clipboard_text("qp_busy_seed")
test result: FAILED. 228 passed; 1 failed; ...
```

**关键观察**:

1. **同一个用例再次失败,但错误信息变了**:上一轮是 `部分格式写入失败 [13]`,
   本轮是 **`OwnershipUnknown`** —— 说明上一轮的注入泄漏修复**确实生效了**,
   现在暴露的是被它掩盖(或未被触达)的**另一个**原因;
2. `OwnershipUnknown` 只由「序列号读到 0」产生;
3. 46 处夹具调用仅 1 处失败,且**两次 CI 失败都是同一个用例**(字母序最靠前的剪贴板用例);
4. 本地(12 核)并行/串行均无法复现。

---

## 1. 排查:一个被**证伪**的假设(重要)

第一直觉是"剪贴板被别的进程/线程占用 → `GetClipboardSequenceNumber` 返回 0"。
**我没有直接采信这个假设,而是写了取证探针验证**,结果**被证伪**:

```
[probe] 空闲时 sequence = 238217
[probe] 剪贴板被他人持有时 sequence = 238217   ← 正常值,不是 0
[probe] 释放后 sequence = 238217
[probe] 结论:持有时是否为 0 = false
```

=> **剪贴板被 `OpenClipboard` 持有时,序列号读取完全正常**,与"被占用"无关。
(探针属一次性排查工具,放在 `test/` 后已删除,未进入提交。)

## 2. 检索权威语义

按 Win32 文档,`GetClipboardSequenceNumber` 的返回值为 **0** 的含义是
**"序列号不可读"**,而不是"剪贴板为空":

> The return value is the clipboard sequence number. If you do not have
> WINSTA_ACCESSCLIPBOARD access to the window station, the function returns zero.

结合 CI 事实:228 个用例通过(其中大量用例需要序列号)→ **访问权整体是可用的**;
失败又固定落在排序最前的剪贴板用例 → 只剩一种解释:
**会话启动期序列号尚未进入可读状态(瞬时态)**。

## 3. 修复(`src/win32/clipboard.rs`)

夹具 `set_raw_clipboard_text_tracked` 增加**有界自愈**:

```rust
for attempt in 1..=3 {
    let expected = clipboard_sequence_number();
    if expected == 0 {
        // 以不依赖序列号的方式建立一次剪贴板状态(open + EmptyClipboard)
        prime_clipboard_session_for_test();
        continue;                      // 重试
    }
    match clipboard_atomic_write(expected, true, ...) { ... }
}
```

- **只做前置条件准备**:读到 0 时先用不依赖序列号的方式(与既有
  `seed_clipboard_formats_with_text` 同一思路)建立一次剪贴板状态,再走
  **与生产完全相同的原子原语** —— 不新建旁路实现;
- **不放宽任何业务断言**:夹具仍必须真的写入成功,否则返回 `None`;
- **有界**:最多 3 次;若 0 是持续性的(如真的无访问权),仍如实失败;
- **失败仍带诊断**:每次失败打印原因与尝试次数,避免再次出现"只知道夹具失败".

新增仅测试使用的确定性钩子 `TEST_SEQ_ZERO_CALLS`(**仅前 N 次不可读**),
与既有的 `TEST_FORCE_SEQ_ZERO`(持续不可读)区分,用于复现"启动瞬时态"。

## 4. 验证:确定性复现 + 变异自证

**新增用例** `test_fixture_self_heals_transient_unreadable_sequence`,两段对照:

1. 钩子令**下一次**读取返回 0 → 夹具必须**自愈**并真的写入成功,
   且断言剪贴板**确实持有**目标文本(防假成功);
2. 持续不可读(`SeqZeroInjection`)→ 夹具必须在有界尝试后**如实返回 None**,
   绝不假成功。

实测输出(证明自愈路径真的走通):

```
[clipboard test fixture] 序列号不可读(=0),先建立一次剪贴板状态后重试(第 1/3 次)
test result: ok            ← 瞬时 0 自愈成功
（对照段:持续 0 → 3 次尝试后如实失败）
```

**变异自证**:把"读到 0 后自愈重试"改为"直接 `return None`" →
`test_fixture_self_heals_transient_unreadable_sequence` **必红**;
还原后全绿。

## 5. 门禁(本机真实执行)

| 项 | 结果 |
|---|---|
| `cargo fmt --check` | 干净 |
| `cargo clippy --all-targets -- -D warnings` | 0 告警 |
| `cargo test --all-targets` | **230 通过 0 失败**(229 → 230,+1 用例) |
| `cargo test --all-targets` 默认并行连跑 2 次 | 每次 230 全绿 |
| `cargo test --all-targets -- --test-threads=1` | **230 通过 0 失败** |
| `cargo audit --no-fetch` | 退出码 0(1329 条 advisory、70 依赖、0 漏洞)。**注**:`cargo audit` 直连 RustSec 镜像出现网络抖动,改用本地缓存库离线校验 |
| `cargo build --release` | 通过(1.2.3) |

## 6. 老实交代的两点

1. **根因是推理 + 旁证,不是 CI 现场复现**:我无法在 CI 机器上直接插桩,
   结论建立在「探针证伪了'被占用'假设」+「访问权整体可用(228 用例通过)」+
   「失败固定落在排序最前的剪贴板用例」这一旁证链上。
   因此本次修复**同时具备**"若真是瞬时态则自愈"与"若是持续态则如实失败并留诊断"
   两条路径 —— 无论哪种解释成立,CI 都不会再出现**无诊断的**单点失败。
2. **未闭合项**:需在 GitHub Actions 上复跑同一提交确认转绿;
   真机验收清单 §10~§15 全部未闭合项不变。

## 7. 与前一轮的关系

| | 上一轮(`6506f93`) | 本轮 |
|---|---|---|
| CI 症状 | `部分格式写入失败 [13]` | `OwnershipUnknown` |
| 根因 | 测试旋钮(注入开关)跨用例泄漏 | 序列号启动期不可读(瞬时) |
| 修复 | RAII guard + 安装断言无残留 | 夹具前置条件有界自愈 + 确定性钩子 |
| 关系 | 上一轮修复**生效**,才让本轮的真因暴露出来(否则错误信息不会变) | 两者独立,互不覆盖 |

**共同教训**:只要夹具/断言失败**不带原因输出**,CI 上的排查就只能靠猜。
本轮之所以能一次区分"注入泄漏"与"序列号不可读",完全依赖上一轮加的诊断输出。