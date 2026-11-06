#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
WindPath 自动化版本更新与 Git Tag 发布工具
功能：
1. 校验目标版本号（自动处理 v 前缀与语义化版本格式）；
2. 检查本地与远程 Git 标签，防止版本冲突；
3. 精准更新 Cargo.toml 中 [package] 的 version 字段；
4. 同步更新 Cargo.lock 中 windpath 的版本号；
5. 执行 Git 提交、打附注标签（v+version）并推送到远程仓库。
"""

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


# 语义化版本正则 (SemVer 2.0.0 规范，兼容带预发布与构建元数据)
SEMVER_REGEX = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-((?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*)(?:\.(?:0|[1-9]\d*|\d*[a-zA-Z-][0-9a-zA-Z-]*))*))?"
    r"(?:\+([0-9a-zA-Z-]+(?:\.[0-9a-zA-Z-]+)*))?$"
)


def run_cmd(cmd: list[str], cwd: Path | None = None, check: bool = True) -> subprocess.CompletedProcess:
    """运行终端命令并返回结果"""
    return subprocess.run(
        cmd,
        cwd=cwd,
        check=check,
        text=True,
        capture_output=True,
        encoding="utf-8",
        errors="replace"
    )


def normalize_version(ver_input: str) -> str:
    """将输入的版本字符串规范化，剔除前导 v/V 及前后空白"""
    cleaned = ver_input.strip()
    if cleaned.lower().startswith("v"):
        cleaned = cleaned[1:].strip()
    return cleaned


def is_valid_semver(ver: str) -> bool:
    """校验是否符合 SemVer 格式"""
    return bool(SEMVER_REGEX.match(ver))


def read_package_version(cargo_toml_path: Path) -> str | None:
    """从 Cargo.toml 的 [package] 节提取当前版本"""
    if not cargo_toml_path.exists():
        return None
    content = cargo_toml_path.read_text(encoding="utf-8")
    in_package = False
    for line in content.splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_package = (stripped == "[package]")
            continue
        if in_package:
            m = re.match(r'^version\s*=\s*"([^"]+)"', stripped)
            if m:
                return m.group(1)
    return None


def update_cargo_toml(cargo_toml_path: Path, new_ver: str) -> tuple[bool, str]:
    """
    精准更新 Cargo.toml 中 [package] 下的 version。
    返回 (是否成功, 旧版本号)。
    """
    content = cargo_toml_path.read_text(encoding="utf-8")
    lines = content.splitlines(keepends=True)
    in_package = False
    found = False
    old_version = ""
    new_lines = []

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_package = (stripped == "[package]")
        if in_package and not found:
            m = re.match(r'^(\s*version\s*=\s*")([^"]+)(".*)$', line)
            if m:
                old_version = m.group(2)
                suffix = "\r\n" if line.endswith("\r\n") else "\n"
                clean_body = m.group(1) + new_ver + m.group(3).rstrip("\r\n")
                line = clean_body + suffix
                found = True
        new_lines.append(line)

    if found:
        cargo_toml_path.write_text("".join(new_lines), encoding="utf-8", newline="")
        return True, old_version
    return False, ""


def update_cargo_lock(cargo_lock_path: Path, pkg_name: str, new_ver: str) -> bool:
    """
    精准更新 Cargo.lock 中指定 package 的 version。
    """
    if not cargo_lock_path.exists():
        return False

    content = cargo_lock_path.read_text(encoding="utf-8")
    lines = content.splitlines(keepends=True)
    in_target_pkg = False
    pkg_pattern = re.compile(rf'^\s*name\s*=\s*"{re.escape(pkg_name)}"\s*$')
    version_pattern = re.compile(r'^(\s*version\s*=\s*")([^"]+)(".*)$')
    found = False
    new_lines = []

    for line in lines:
        stripped = line.strip()
        if stripped == "[[package]]":
            in_target_pkg = False
        elif in_target_pkg and not found:
            m = version_pattern.match(line)
            if m:
                suffix = "\r\n" if line.endswith("\r\n") else "\n"
                line = m.group(1) + new_ver + m.group(3).rstrip("\r\n") + suffix
                found = True
                in_target_pkg = False
        elif not in_target_pkg and pkg_pattern.match(stripped):
            in_target_pkg = True

        new_lines.append(line)

    if found:
        cargo_lock_path.write_text("".join(new_lines), encoding="utf-8", newline="")
        return True
    return False


def get_git_branch(root_dir: Path) -> str:
    """获取当前 Git 分支名称"""
    res = run_cmd(["git", "branch", "--show-current"], cwd=root_dir, check=False)
    if res.returncode == 0 and res.stdout.strip():
        return res.stdout.strip()
    return "main"


def check_tag_exists(tag_name: str, root_dir: Path) -> tuple[bool, bool]:
    """
    检查标签是否存在。
    返回 (本地是否存在, 远程是否存在)。
    """
    # 检查本地
    local_res = run_cmd(["git", "tag", "-l", tag_name], cwd=root_dir, check=False)
    local_exists = (local_res.returncode == 0 and tag_name in [t.strip() for t in local_res.stdout.splitlines()])

    # 检查远程
    remote_exists = False
    remote_res = run_cmd(["git", "ls-remote", "--tags", "origin", f"refs/tags/{tag_name}"], cwd=root_dir, check=False)
    if remote_res.returncode == 0 and remote_res.stdout.strip():
        remote_exists = True

    return local_exists, remote_exists


def get_uncommitted_changes(root_dir: Path) -> list[str]:
    """获取当前工作区所有改动（包括未跟踪文件）"""
    res = run_cmd(["git", "status", "--porcelain"], cwd=root_dir, check=False)
    if res.returncode == 0 and res.stdout.strip():
        return [line.strip() for line in res.stdout.splitlines() if line.strip()]
    return []


def prompt_yes_no(prompt: str, default: bool = True) -> bool:
    """交互询问 [Y/n] 或 [y/N]"""
    choice = " [Y/n]: " if default else " [y/N]: "
    try:
        ans = input(prompt + choice).strip().lower()
    except (EOFError, KeyboardInterrupt):
        print("\n操作已取消。")
        sys.exit(0)
    if not ans:
        return default
    return ans in ("y", "yes")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="WindPath 自动修改版本号、打 Git Tag 并提交推送工具"
    )
    parser.add_argument("version", nargs="?", help="新的版本号（如 1.2.7 或 v1.2.7）")
    parser.add_argument("-m", "--message", help="发布附注说明（默认为 Release v{version}）")
    parser.add_argument("-y", "--yes", action="store_true", help="跳过确认，全自动执行")
    parser.add_argument("--no-push", action="store_true", help="仅修改、提交并打 Tag，不推送到远程")
    parser.add_argument("--all", "-a", action="store_true", help="自动暂存工作区所有改动（等同于 git add -A）")

    args = parser.parse_args()

    root_dir = Path(__file__).resolve().parent
    cargo_toml = root_dir / "Cargo.toml"
    cargo_lock = root_dir / "Cargo.lock"

    if not cargo_toml.exists():
        print(f"❌ 错误: 未在 {root_dir} 找到 Cargo.toml 文件！")
        sys.exit(1)

    # 1. 检查 Git 环境
    git_check = run_cmd(["git", "rev-parse", "--is-inside-work-tree"], cwd=root_dir, check=False)
    if git_check.returncode != 0:
        print("❌ 错误: 当前目录不是有效的 Git 仓库！")
        sys.exit(1)

    current_version = read_package_version(cargo_toml)
    if not current_version:
        print("❌ 错误: 无法从 Cargo.toml 中解析出 [package] 版本号！")
        sys.exit(1)

    # 2. 获取目标版本号
    target_ver_raw = args.version
    if not target_ver_raw:
        print("=" * 60)
        print("   WindPath 自动化版本发布工具")
        print("=" * 60)
        print(f"📌 当前 Cargo.toml 版本为: {current_version}")
        while True:
            try:
                target_ver_raw = input(f"请输入新的版本号 (例如 1.2.7): ").strip()
            except (EOFError, KeyboardInterrupt):
                print("\n操作已取消。")
                sys.exit(0)
            if target_ver_raw:
                break
            print("⚠️ 版本号不能为空，请重新输入！")

    new_version = normalize_version(target_ver_raw)
    if not is_valid_semver(new_version):
        print(f"❌ 错误: '{target_ver_raw}' 不是合法的语义化版本号 (例如 1.2.7 或 1.2.7-beta.1)！")
        sys.exit(1)

    tag_name = f"v{new_version}"

    if new_version == current_version:
        print(f"⚠️ 警告: 输入的新版本号 ({new_version}) 与当前版本号相同！")
        if not args.yes and not prompt_yes_no("确定要继续使用相同版本号重新打标签发布吗？", default=False):
            print("已取消发布。")
            sys.exit(0)

    # 3. 检查 Tag 是否已存在
    local_has_tag, remote_has_tag = check_tag_exists(tag_name, root_dir)
    if local_has_tag or remote_has_tag:
        conflict_msg = []
        if local_has_tag:
            conflict_msg.append("本地仓库")
        if remote_has_tag:
            conflict_msg.append("远程仓库 origin")
        print(f"❌ 错误: Git 标签 '{tag_name}' 已经在 {' 和 '.join(conflict_msg)} 中存在！")
        print("请检查版本号，避免重复打标覆盖历史发布。")
        sys.exit(1)

    current_branch = get_git_branch(root_dir)

    # 4. 工作区改动检查
    changes = get_uncommitted_changes(root_dir)
    stage_all = args.all
    if changes and not args.yes and not stage_all:
        print("\n🔍 检测到当前工作区有以下改动：")
        for ch in changes[:10]:
            print(f"   {ch}")
        if len(changes) > 10:
            print(f"   ... 还有 {len(changes) - 10} 个文件")
        stage_all = prompt_yes_no("是否将工作区的所有未提交改动一并打包提交？", default=True)

    # 5. 打印发布计划并确认
    commit_msg = f"release: {tag_name}"
    tag_msg = args.message if args.message else f"Release {tag_name}"
    push_remote = not args.no_push

    print("\n" + "=" * 60)
    print("📋 发布计划清单：")
    print("=" * 60)
    print(f"  • 当前版本:    {current_version}")
    print(f"  • 发布新版本:  {new_version}")
    print(f"  • Git 标签名:  {tag_name}")
    print(f"  • 提交信息:    {commit_msg}")
    print(f"  • 标签附注:    {tag_msg}")
    print(f"  • 暂存范围:    {'全量变更 (git add -A)' if stage_all else '仅版本控制文件 (Cargo.toml / Cargo.lock)'}")
    print(f"  • 当前分支:    {current_branch}")
    print(f"  • 推送远程:    {'是 (origin ' + current_branch + ' & ' + tag_name + ')' if push_remote else '否'}")
    print("=" * 60)

    if not args.yes:
        if not prompt_yes_no("确认执行以上发布流程？", default=True):
            print("操作已取消。")
            sys.exit(0)

    # 6. 更新 Cargo.toml
    print(f"\n📝 正在更新 Cargo.toml 中的版本号 -> {new_version} ...")
    success, old_ver = update_cargo_toml(cargo_toml, new_version)
    if not success:
        print("❌ 错误: 无法在 Cargo.toml 的 [package] 节中替换版本号！")
        sys.exit(1)
    print("   ✓ Cargo.toml 更新成功。")

    # 7. 更新 Cargo.lock
    if cargo_lock.exists():
        print("📝 正在同步 Cargo.lock 中的 windpath 版本号 ...")
        update_cargo_lock(cargo_lock, "windpath", new_version)
        print("   ✓ Cargo.lock 文本版本号同步完成。")

    # 尝试 cargo check 校验锁文件一致性（若系统安装了 cargo）
    cargo_check = run_cmd(["cargo", "check", "--quiet"], cwd=root_dir, check=False)
    if cargo_check.returncode == 0:
        print("   ✓ cargo 校验通过，Cargo.lock 状态一致。")

    # 8. Git 暂存与提交
    print("\n📦 正在执行 Git 提交 ...")
    if stage_all:
        run_cmd(["git", "add", "-A"], cwd=root_dir)
    else:
        run_cmd(["git", "add", "Cargo.toml"], cwd=root_dir)
        if cargo_lock.exists():
            run_cmd(["git", "add", "Cargo.lock"], cwd=root_dir)

    commit_res = run_cmd(["git", "commit", "-m", commit_msg], cwd=root_dir, check=False)
    if commit_res.returncode != 0:
        # 如果没有内容可提交（例如版本号未变且无改动）
        if "nothing to commit" in commit_res.stdout or "无文件要提交" in commit_res.stdout:
            print("   ℹ️ 没有需要提交的更改，跳过 commit。")
        else:
            print(f"❌ Git 提交失败:\n{commit_res.stderr}")
            sys.exit(1)
    else:
        print(f"   ✓ 已提交: {commit_msg}")

    # 9. 打 Git Tag
    print(f"\n🏷️ 正在创建附注标签 {tag_name} ...")
    tag_res = run_cmd(["git", "tag", "-a", tag_name, "-m", tag_msg], cwd=root_dir, check=False)
    if tag_res.returncode != 0:
        print(f"❌ 创建标签失败:\n{tag_res.stderr}")
        sys.exit(1)
    print(f"   ✓ 标签 {tag_name} 创建成功。")

    # 10. 推送到远程
    if push_remote:
        print(f"\n🚀 正在推送到远程 origin (分支: {current_branch}, 标签: {tag_name}) ...")
        push_branch = run_cmd(["git", "push", "origin", current_branch], cwd=root_dir, check=False)
        if push_branch.returncode != 0:
            print(f"❌ 推送分支 {current_branch} 失败:\n{push_branch.stderr}")
            print(f"⚠️ 请在网络通畅后手动执行: git push origin {current_branch} && git push origin {tag_name}")
            sys.exit(1)
        print(f"   ✓ 分支 {current_branch} 推送成功。")

        push_tag = run_cmd(["git", "push", "origin", tag_name], cwd=root_dir, check=False)
        if push_tag.returncode != 0:
            print(f"❌ 推送标签 {tag_name} 失败:\n{push_tag.stderr}")
            print(f"⚠️ 请在网络通畅后手动执行: git push origin {tag_name}")
            sys.exit(1)
        print(f"   ✓ 标签 {tag_name} 推送成功！")

    print("\n" + "=" * 60)
    print(f"🎉 版本 {tag_name} 发布成功！")
    if push_remote:
        print("💡 GitHub Actions 的 release 流水线已被触发，可前往仓库 Releases 查看构建进度。")
    print("=" * 60)


if __name__ == "__main__":
    main()

