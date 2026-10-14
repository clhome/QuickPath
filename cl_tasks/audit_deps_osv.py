#!/usr/bin/env python3
"""依赖漏洞审计（OSV 兜底通道）。

背景：报告 §12 / §14 要求 `cargo audit` 无未处理高危漏洞。本项目开发机无法访问
github.com（RustSec/advisory-db 是 GitHub 仓库），`cargo audit` 因此无法拉取
advisory 数据库。本脚本改用 Google OSV API（api.osv.dev，聚合了 RustSec 与 GHSA
等来源的 crates.io 公告）做等价的依赖漏洞查询，作为本机的兜底验证手段；
CI（GitHub runner）仍以 `cargo audit` 为准。

用法：
    python cl_tasks/audit_deps_osv.py [--lock Cargo.lock] [--json]

退出码：0 = 未发现漏洞；1 = 发现漏洞（列出）；2 = 环境/网络错误。
仅使用标准库（urllib + json），不依赖第三方包。
"""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request

OSV_BATCH_URL = "https://api.osv.dev/v1/querybatch"
BATCH_SIZE = 200


def parse_lockfile(path: str) -> list[tuple[str, str]]:
    """从 Cargo.lock 里取出 (name, version)。

    Cargo.lock 是受限的 TOML 子集：每个依赖是一个 `[[package]]` 段，
    段内 `name = "..."` / `version = "..."`。用逐行解析即可，避免引入 toml 依赖。
    """
    packages: list[tuple[str, str]] = []
    name = None
    version = None
    in_package = False

    with open(path, encoding="utf-8") as handle:
        for raw in handle:
            line = raw.strip()
            if line.startswith("["):
                if in_package and name and version:
                    packages.append((name, version))
                in_package = line == "[[package]]"
                name = version = None
                continue
            if not in_package:
                continue
            if line.startswith("name = "):
                name = line.split("=", 1)[1].strip().strip('"')
            elif line.startswith("version = "):
                version = line.split("=", 1)[1].strip().strip('"')

    if in_package and name and version:
        packages.append((name, version))
    return packages


def query_osv(packages: list[tuple[str, str]]) -> dict[tuple[str, str], list[dict]]:
    """批量查询 OSV；返回 {(name, version): [漏洞对象...]}（只含非空结果）。"""
    findings: dict[tuple[str, str], list[dict]] = {}
    for start in range(0, len(packages), BATCH_SIZE):
        chunk = packages[start : start + BATCH_SIZE]
        payload = {
            "queries": [
                {"package": {"name": name, "ecosystem": "crates.io"}, "version": version}
                for name, version in chunk
            ]
        }
        request = urllib.request.Request(
            OSV_BATCH_URL,
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=60) as response:
            body = json.loads(response.read())
        for (name, version), result in zip(chunk, body.get("results", [])):
            vulns = result.get("vulns") or []
            if vulns:
                findings[(name, version)] = vulns
    return findings


def severity_of(vuln: dict) -> str:
    """尽力给出严重度：OSV 的 database_specific.severity 或 CVSS 向量存在性。"""
    specific = vuln.get("database_specific") or {}
    if isinstance(specific.get("severity"), str):
        return specific["severity"]
    if vuln.get("severity"):
        return "CVSS"
    return "unknown"


def main() -> int:
    parser = argparse.ArgumentParser(description="OSV 依赖漏洞审计（cargo audit 兜底）")
    parser.add_argument("--lock", default="Cargo.lock", help="Cargo.lock 路径")
    parser.add_argument("--json", action="store_true", help="以 JSON 输出结果")
    args = parser.parse_args()

    try:
        packages = parse_lockfile(args.lock)
    except OSError as error:
        print(f"无法读取 {args.lock}: {error}", file=sys.stderr)
        return 2

    if not packages:
        print(f"{args.lock} 中未解析到任何依赖（解析逻辑或文件格式异常）", file=sys.stderr)
        return 2

    try:
        findings = query_osv(packages)
    except (urllib.error.URLError, TimeoutError) as error:
        print(f"OSV 查询失败（网络不可达？）: {error}", file=sys.stderr)
        return 2

    if args.json:
        print(
            json.dumps(
                {
                    "packages_scanned": len(packages),
                    "vulnerable": {
                        f"{name}@{version}": [
                            {
                                "id": vuln.get("id"),
                                "summary": vuln.get("summary"),
                                "severity": severity_of(vuln),
                                "aliases": vuln.get("aliases", []),
                            }
                            for vuln in vulns
                        ]
                        for (name, version), vulns in findings.items()
                    },
                },
                ensure_ascii=False,
                indent=2,
            )
        )
    else:
        print(f"已扫描 {len(packages)} 个依赖（OSV / crates.io）")
        if not findings:
            print("未发现已知漏洞（0 条）")
        else:
            for (name, version), vulns in sorted(findings.items()):
                for vuln in vulns:
                    print(
                        f"[{severity_of(vuln)}] {name}@{version} {vuln.get('id')}: "
                        f"{vuln.get('summary', '')}"
                    )
            print(f"合计 {sum(len(v) for v in findings.values())} 条漏洞记录")

    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
