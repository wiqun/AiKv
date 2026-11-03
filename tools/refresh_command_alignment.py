#!/usr/bin/env python3
"""刷新 aikv/tests/fixtures 里的命令对齐清单.

Redis 清单默认读环境变量 REDIS_COMMANDS_DIR, 否则沿用已提交的
tests/fixtures/redis_8_8_3_commands.txt. 那个文件来自 Redis tag 8.8.3 的
src/commands/*.json, 只收顶层命令名.

业务代码清单扫描可通过环境变量 BUSINESS_SCAN_ROOTS (路径以冒号或逗号分隔)
或 --scan-dir 参数指定目录, 扫描其中的 .cs / .lua 脚本 (去掉注释).
未指定或目录不存在时, 保留已提交的 business_*.txt 清单, 避免误覆盖.
"""

from __future__ import annotations

import argparse
import json
import os
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "fixtures"
REGISTRY = ROOT / "src" / "command" / "registry.rs"
EXECUTE = ROOT / "src" / "command" / "script" / "execute" / "mod.rs"

# 线上已观察到的脚本命令
OBSERVED_SCRIPT = ["BITFIELD"]

METHOD_COMMANDS = {
    "StringSetBitAsync": "SETBIT",
    "StringGetBitAsync": "GETBIT",
    "StringBitCountAsync": "BITCOUNT",
}

CALL_RE = re.compile(
    r"""redis\.(?:pcall|call)\s*\(\s*['"]([A-Za-z][A-Za-z0-9_.]*)['"]""",
    re.IGNORECASE,
)
EXEC_RE = re.compile(
    r"""Execute(?:Async)?\s*\(\s*["']([A-Za-z][A-Za-z0-9_.]*)["']"""
)
CMD_RE = re.compile(r'cmd!\("([^"]+)",\s*[^,]+,\s*&\[([^\]]*)\]')


def strip_comments(text: str) -> str:
    out: list[str] = []
    i = 0
    n = len(text)
    while i < n:
        if text.startswith("//", i):
            nxt = text.find("\n", i)
            i = n if nxt < 0 else nxt
            continue
        if text.startswith("/*", i):
            nxt = text.find("*/", i + 2)
            i = n if nxt < 0 else nxt + 2
            continue
        if text[i] in "\"'":
            quote = text[i]
            out.append(quote)
            i += 1
            while i < n:
                if text[i] == "\\":
                    out.append(text[i : i + 2])
                    i += 2
                    continue
                out.append(text[i])
                if text[i] == quote:
                    i += 1
                    break
                i += 1
            continue
        out.append(text[i])
        i += 1
    return "".join(out)


def write_names(path: Path, header: str, items: list[str]) -> None:
    body = "\n".join(items)
    suffix = "\n" if items else ""
    path.write_text(f"{header.rstrip()}\n{body}{suffix}", encoding="utf-8")


def load_redis_file(path: Path) -> dict[str, str]:
    table: dict[str, str] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        name, kind = line.split()
        table[name] = kind
    return table


def redis_from_commands_dir(commands_dir: Path) -> dict[str, str]:
    table: dict[str, str] = {}
    for path in sorted(commands_dir.glob("*.json")):
        data = json.loads(path.read_text(encoding="utf-8"))
        for name, spec in data.items():
            if " " in name or spec.get("container"):
                continue
            flags = spec.get("command_flags") or []
            kind = "NOSCRIPT" if "NOSCRIPT" in flags else "SCRIPT"
            previous = table.get(name)
            if previous is not None and previous != kind:
                raise SystemExit(f"conflicting flags for {name}")
            table[name] = kind
    return table


def parse_registry() -> dict[str, bool]:
    text = REGISTRY.read_text(encoding="utf-8")
    found: dict[str, bool] = {}
    for name, flags in CMD_RE.findall(text):
        found[name] = "noscript" in flags
    return found


def parse_allowlist() -> list[str]:
    text = EXECUTE.read_text(encoding="utf-8")
    match = re.search(r"SCRIPT_COMMANDS: &\[&str\] = &\[(.*?)\];", text, re.S)
    if not match:
        raise SystemExit("SCRIPT_COMMANDS not found")
    return re.findall(r'"([^"]+)"', match.group(1))


def parse_scan_roots(cli_dirs: list[str] | None) -> list[Path]:
    roots: list[Path] = []
    if cli_dirs:
        for d in cli_dirs:
            p = Path(d).expanduser()
            if p.exists():
                roots.append(p)
            else:
                print(f"Warning: specified scan dir not found: {p}")
    env_dirs = os.environ.get("BUSINESS_SCAN_ROOTS")
    if env_dirs:
        for raw in re.split(r"[:;,]", env_dirs):
            raw = raw.strip()
            if not raw:
                continue
            p = Path(raw).expanduser()
            if p.exists():
                roots.append(p)
            else:
                print(f"Warning: env scan dir not found: {p}")
    return roots


def scan_business(
    roots: list[Path], redis_names: set[str]
) -> tuple[list[str], list[str], list[str]]:
    script: set[str] = set(OBSERVED_SCRIPT)
    direct: set[str] = set()
    for root in roots:
        for path in root.rglob("*"):
            if path.suffix.lower() not in {".cs", ".lua"} or not path.is_file():
                continue
            if any(part in {"bin", "obj", ".git"} for part in path.parts):
                continue
            text = strip_comments(path.read_text(encoding="utf-8", errors="ignore"))
            script.update(item.group(1).upper() for item in CALL_RE.finditer(text))
            direct.update(item.group(1).upper() for item in EXEC_RE.finditer(text))
            for method, command in METHOD_COMMANDS.items():
                if method in text:
                    direct.add(command)
    noise = sorted(name for name in script if name not in redis_names)
    return sorted(script), sorted(direct), noise


def main() -> None:
    parser = argparse.ArgumentParser(description="刷新 Redis 与业务命令对齐清单")
    parser.add_argument(
        "--scan-dir",
        action="append",
        dest="scan_dirs",
        help="指定业务代码扫描根目录 (可重复指定)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="仅校验现有清单是否与代码对齐, 不写入文件",
    )
    args = parser.parse_args()

    FIXTURES.mkdir(parents=True, exist_ok=True)
    commands_dir = os.environ.get("REDIS_COMMANDS_DIR")
    redis_path = FIXTURES / "redis_8_8_3_commands.txt"
    if commands_dir:
        redis = redis_from_commands_dir(Path(commands_dir))
        if not args.check:
            write_names(
                redis_path,
                "# Redis 8.8.3 顶层命令. 来源: tag 8.8.3 src/commands/*.json\n"
                "# NAME SCRIPT|NOSCRIPT",
                [f"{name}\t{redis[name]}" for name in sorted(redis)],
            )
    elif redis_path.exists():
        redis = load_redis_file(redis_path)
    else:
        raise SystemExit("set REDIS_COMMANDS_DIR or commit redis_8_8_3_commands.txt")

    registry = parse_registry()
    allow = set(parse_allowlist())
    redis_names = set(redis)

    unsupported = sorted(name for name in redis if name not in registry)
    extensions = sorted(name for name in registry if name not in redis)
    gaps: list[str] = []
    for name, aikv_noscript in sorted(registry.items()):
        if aikv_noscript or redis.get(name) == "NOSCRIPT" or name in allow:
            continue
        gaps.append(name)

    if not args.check:
        write_names(
            FIXTURES / "redis_unsupported.txt",
            "# Redis 8.8.3 有, AiKv 注册表没有的顶层命令.",
            unsupported,
        )
        write_names(
            FIXTURES / "aikv_extensions.txt",
            "# AiKv 注册表有, Redis 8.8.3 顶层命令没有的名字.",
            extensions,
        )
        write_names(
            FIXTURES / "script_command_gaps.txt",
            "# 已实现, 且 Redis 允许在脚本里调用, 但 redis.call 白名单还没有.\n"
            "# 留在这里的是阻塞, 管理, 整库扫描, 跨节点迁移, 以及脚本事务里没有对应语义的命令.",
            gaps,
        )

    scan_roots = parse_scan_roots(args.scan_dirs)
    if scan_roots:
        script, direct, noise = scan_business(scan_roots, redis_names)
        if not args.check:
            write_names(
                FIXTURES / "business_script_commands.txt",
                "# redis.call / redis.pcall 字面量, 加上线上观察到的 BITFIELD.\n"
                "# 不是 Redis 命令的名字同时记在 business_command_noise.txt.",
                script,
            )
            write_names(
                FIXTURES / "business_direct_commands.txt",
                "# ExecuteAsync 字面量, 以及会发出命令名的客户端方法.",
                direct,
            )
            write_names(
                FIXTURES / "business_command_noise.txt",
                "# 扫到了, 但不是 Redis 8.8.3 命令. 目前是 Lua 解析器单测里的 INC.",
                noise,
            )
        print(
            f"redis={len(redis)} unsupported={len(unsupported)} "
            f"extensions={len(extensions)} gaps={len(gaps)} "
            f"script={len(script)} direct={len(direct)} noise={len(noise)} (scanned {len(scan_roots)} roots)"
        )
    else:
        # 未指定或目录不存在时, 保持已提交的 fixtures, 不写空文件
        print(
            f"redis={len(redis)} unsupported={len(unsupported)} "
            f"extensions={len(extensions)} gaps={len(gaps)} "
            f"(business scan skipped: no valid roots provided via --scan-dir or BUSINESS_SCAN_ROOTS)"
        )


if __name__ == "__main__":
    main()
