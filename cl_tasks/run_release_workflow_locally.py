#!/usr/bin/env python3
"""在本地忠实执行 `.github/workflows/release.yml` 的步骤序列（取证用工具）。

## 为什么需要它

发布通道由 GitHub Actions 承载，而本机无法访问 github.com（也就无法 `push` tag 触发真实运行）。
本工具**不替代** GitHub Actions，它做的是：把 workflow YAML 里**每一段 `run:` 脚本原文**
（不重写、不改写）在本地按同样的顺序、同样的 tag 上下文、同样的 shell（`pwsh`）执行一遍，
从而验证「workflow 自身的脚本逻辑能否端到端跑通、产物与 SHA-256 清单是否正确」。

## 明确不覆盖（必须在真实 GitHub 运行中才能验证）

* `uses:` 步骤（`actions/checkout`、`Swatinem/rust-cache`、`softprops/action-gh-release`）；
* GitHub Runner 的环境差异（windows-latest 镜像、预装工具链、缓存命中行为）；
* Release 资产上传与 `GITHUB_TOKEN` 权限；
* `environment: release` 的审批/保护规则（仓库 Settings 侧配置）。

## 用法

    python cl_tasks/run_release_workflow_locally.py --tag v1.2.3
    python cl_tasks/run_release_workflow_locally.py --tag v1.2.3 --log cl_tasks/xxx.log

退出码 0 = 所有 `run:` 步骤均按预期成功（含清单自校验）。
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    import yaml
except ImportError:  # pragma: no cover
    sys.exit("需要 PyYAML：pip install pyyaml")

# Windows 控制台默认可能是 GBK（cp936）：步骤输出包含非 GBK 字符时 print 会抛
# UnicodeEncodeError 而中断整个演练（踩过一次）。统一把 stdout/stderr 切到 UTF-8，
# 并用 errors="replace" 兜底，保证工具本身永远不会因控制台编码而失败。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except (AttributeError, OSError):  # pragma: no cover - 非文本流
        pass

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "release.yml"


class Log:
    """同时写 stdout 与（可选的）日志文件。"""

    def __init__(self, path: Path | None) -> None:
        self._fh = path.open("w", encoding="utf-8", newline="\n") if path else None

    def __call__(self, line: str = "") -> None:
        print(line)
        if self._fh:
            self._fh.write(line + "\n")

    def close(self) -> None:
        if self._fh:
            self._fh.close()


def find_pwsh() -> str:
    """CI 用的是 `shell: pwsh`（PowerShell Core）。优先 pwsh，退化为 Windows PowerShell。"""
    for candidate in ("pwsh", "powershell"):
        if shutil.which(candidate):
            return candidate
    sys.exit("未找到 pwsh / powershell")


def substitute(script: str, tag: str) -> str:
    """替换 workflow 上下文表达式（本工具只支持 release.yml 用到的这几个）。"""
    return script.replace("${{ github.ref_name }}", tag)


def run_ps(script: str, pwsh: str, env: dict[str, str]) -> tuple[int, str]:
    """把步骤脚本写入临时 .ps1 后用 pwsh 执行（避免命令行引号/编码问题）。"""
    with tempfile.NamedTemporaryFile("w", suffix=".ps1", encoding="utf-8", newline="\n",
                                     delete=False) as fh:
        fh.write(script)
        tmp = fh.name
    try:
        proc = subprocess.run(
            [pwsh, "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", tmp],
            cwd=REPO_ROOT, env=env, capture_output=True, text=True, encoding="utf-8",
            errors="replace", timeout=3600,
        )
        return proc.returncode, (proc.stdout or "") + (proc.stderr or "")
    finally:
        os.unlink(tmp)


def verify_manifest(dist: Path, log: Log) -> bool:
    """用 Python 独立复算 dist/sha256sums.txt 里的每个哈希（与被测程序无关的第二实现）。"""
    manifest = dist / "sha256sums.txt"
    if not manifest.exists():
        log("  [清单自校验] 失败：dist/sha256sums.txt 不存在")
        return False
    lines = [ln for ln in manifest.read_text(encoding="ascii").splitlines() if ln.strip()]
    if not lines:
        log("  [清单自校验] 失败：清单为空（这正是 2026-10-14 修复过的缺陷形态）")
        return False
    ok = True
    for line in lines:
        parts = line.split(None, 1)
        if len(parts) != 2:
            log(f"  [清单自校验] 失败：无法解析行 {line!r}")
            ok = False
            continue
        want, name = parts[0].strip(), parts[1].strip()
        target = dist / name
        if not target.exists():
            log(f"  [清单自校验] 失败：清单引用的文件不存在 -> {name}")
            ok = False
            continue
        got = hashlib.sha256(target.read_bytes()).hexdigest()
        if got == want:
            log(f"  [清单自校验] OK  {name}  {got}")
        else:
            log(f"  [清单自校验] 失败 {name}\n    清单={want}\n    复算={got}")
            ok = False
    return ok


def main() -> int:
    ap = argparse.ArgumentParser(description="本地忠实执行 release.yml 的 run: 步骤")
    ap.add_argument("--tag", required=True, help="模拟的 tag，例如 v1.2.3 或 v1.2.3-rc1")
    ap.add_argument("--workflow", default=str(DEFAULT_WORKFLOW))
    ap.add_argument("--log", default=None, help="把完整输出同时写入该文件")
    args = ap.parse_args()

    wf_path = Path(args.workflow)
    raw = wf_path.read_bytes()
    doc = yaml.safe_load(raw.decode("utf-8"))
    # PyYAML 按 YAML 1.1 把顶层 `on` 解析成布尔 True，属已知怪癖，不影响步骤读取。
    job = next(iter(doc["jobs"].values()))
    steps = job["steps"]

    log = Log(Path(args.log) if args.log else None)
    pwsh = find_pwsh()

    log("=" * 78)
    log(f"本地忠实执行 workflow：{wf_path.relative_to(REPO_ROOT)}")
    log(f"模拟 tag（github.ref_name）= {args.tag}")
    log(f"shell = {pwsh} ；cwd = {REPO_ROOT}")
    log(f"workflow 文件 sha256 = {hashlib.sha256(raw).hexdigest()}")
    log(f"步骤总数 = {len(steps)}（其中 run: 步骤将被真实执行，uses: 步骤跳过）")
    log("=" * 78)

    env = dict(os.environ)
    env["GITHUB_REF_NAME"] = args.tag
    gh_output = tempfile.NamedTemporaryFile("w", suffix=".txt", delete=False)
    gh_output.close()
    env["GITHUB_OUTPUT"] = gh_output.name

    skipped: list[str] = []
    ran = 0
    failures: list[str] = []
    start_all = time.time()

    try:
        for idx, step in enumerate(steps, start=1):
            name = step.get("name", f"<step {idx}>")
            log("")
            log(f"───── [{idx}/{len(steps)}] {name}")

            if "uses" in step:
                skipped.append(f"{name}  →  uses: {step['uses']}")
                log(f"  [跳过] uses: {step['uses']}（GitHub Action，本地无法执行）")
                continue

            script = step.get("run")
            if not script:
                log("  [跳过] 该步骤既无 run 也无 uses")
                continue

            shell = step.get("shell", "bash")
            log(f"  [执行] shell={shell}（本地统一用 pwsh；本 workflow 的 run 步骤全部声明为 pwsh）")
            log("  ---- 步骤脚本原文（未改写）----")
            for ln in substitute(script, args.tag).splitlines():
                log(f"  | {ln}")
            log("  ---- 执行输出 ----")

            t0 = time.time()
            code, out = run_ps(substitute(script, args.tag), pwsh, env)
            for ln in out.splitlines():
                log(f"  > {ln}")
            log(f"  [结果] exit={code}  用时={time.time() - t0:.1f}s")
            ran += 1
            if code != 0:
                failures.append(f"{name}（exit={code}）")
                log("  !! 步骤失败，按 CI 语义立即中止后续步骤")
                break

        # ── 附加：清单独立复算（不属于 workflow，属本工具的交叉校验）──────────
        dist = REPO_ROOT / "dist"
        manifest_ok = None
        if dist.exists() and not failures:
            log("")
            log("───── [附加] SHA-256 清单独立复算（Python hashlib 第二实现）")
            manifest_ok = verify_manifest(dist, log)
            log(f"  [清单自校验] 结论：{'全部一致' if manifest_ok else '存在问题'}")
            log("  dist/ 实际内容：")
            for p in sorted(dist.iterdir()):
                log(f"    {p.name}  ({p.stat().st_size} bytes)")
    finally:
        gh_output.close()
        os.unlink(gh_output.name)

    log("")
    log("=" * 78)
    log(f"总计：真实执行 run: 步骤 {ran} 个；跳过 uses: 步骤 {len(skipped)} 个；"
        f"总用时 {time.time() - start_all:.1f}s")
    if skipped:
        log("跳过的步骤（需真实 GitHub 运行才能验证）：")
        for s in skipped:
            log(f"  - {s}")
    if failures:
        log(f"结论：**失败** —— {failures}")
    elif manifest_ok is False:
        log("结论：**失败** —— 所有 run: 步骤通过，但 SHA-256 清单独立复算不一致")
    else:
        log("结论：**通过** —— workflow 的全部 run: 步骤按顺序成功；产物与 SHA-256 清单经独立复算一致")
    log("注意：本结果不覆盖 uses: 步骤、Runner 环境差异、Release 资产上传与 "
        "environment 审批保护（报告 §3.2 第 7 项）。")
    log("=" * 78)
    log.close()

    return 1 if (failures or manifest_ok is False) else 0


if __name__ == "__main__":
    raise SystemExit(main())
