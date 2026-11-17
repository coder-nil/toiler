#!/usr/bin/env python3
"""一键把 Toiler 的版本号升到下一个版本。

为什么需要它：版本号散落在 8 个文件里，漏改任何一处都会让「关于」面板、
安装包、`/api` 自报版本三者对不上。`tauri.conf.json` 是发布流水线的唯一
校验对象（`.github/workflows/release.yml` 会核 tag），所以本脚本以它为基数，
其余文件全部对齐到它。

会改这些文件：

| 文件 | 位置 |
|---|---|
| `crates/toiler-{poc,session,server,desktop}/Cargo.toml` | `[package]` 的 `version` |
| `Cargo.lock` | 4 个 `toiler-*` 包的 `version`（走 `cargo update`） |
| `crates/toiler-desktop/tauri.conf.json` | 顶层 `version`（**基数**） |
| `crates/toiler-desktop/frontend/package.json` | npm 包版本 |
| `crates/toiler-desktop/frontend/package-lock.json` | 顶层 + `packages[""]` 两处 |
| `CHANGELOG.md` | 新增 `## [新版本] - 今天` 段 |
| `crates/toiler-desktop/frontend/data/changelog.json` | 同上（界面读这份） |

用法：

    tools/bump-version.py                # 升 patch：1.0.4 → 1.0.5
    tools/bump-version.py 1.2.0          # 指定版本
    tools/bump-version.py --minor        # 1.0.4 → 1.1.0
    tools/bump-version.py --major        # 1.0.4 → 2.0.0
    tools/bump-version.py --dry-run      # 只打印将要改什么，不落盘
    tools/bump-version.py --check        # 只校验各文件版本是否一致（退出码 1 = 不一致）
    tools/bump-version.py --no-lock      # 跳过 cargo update（改完自己跑）
    tools/bump-version.py --no-changelog # 不写变更记录（自己补内容）
    tools/bump-version.py --since v1.0.5 # 显式指定变更记录的起始发布标签

变更记录默认覆盖当前历史中最近的 `v<semver>` 发布标签之后到 HEAD 的全部提交，
包括合并分支的提交、提交正文与哈希；没有发布标签时覆盖完整历史。未提交改动
不会被当作提交收录，浅克隆则拒绝生成，避免静默遗漏。`--dry-run` 同时预览内容。

脚本**不**提交、**不**打 tag、**不**重建前端产物。收尾动作会打印出来。

两个必须知道的点：

1. **前端产物里嵌着版本号**（`next.config.ts` 把 `package.json` 的 version 注入
   `NEXT_PUBLIC_*`）。改完不 `npm run build`，二进制里还是旧版本字符串。
2. **`data/changelog.json` 不能由 `CHANGELOG.md` 重新生成**：历史条目与 Markdown
   已有漂移（个别小节的条目顺序、重复项不同），重写会产生无关 diff。所以本脚本
   是「往两份里各插一段」，不是「由 Markdown 生成 JSON」。
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# 基准文件：发布流水线校验 tag 与它一致，所以以它为准。
BASE_FILE = ROOT / "crates/toiler-desktop/tauri.conf.json"

CARGO_TOMLS = [
    ROOT / "crates/toiler-poc/Cargo.toml",
    ROOT / "crates/toiler-session/Cargo.toml",
    ROOT / "crates/toiler-server/Cargo.toml",
    ROOT / "crates/toiler-desktop/Cargo.toml",
]

CARGO_LOCK = ROOT / "Cargo.lock"

FRONTEND_DIR = ROOT / "crates/toiler-desktop/frontend"
PACKAGE_JSON = FRONTEND_DIR / "package.json"
PACKAGE_LOCK = FRONTEND_DIR / "package-lock.json"

CHANGELOG_MD = ROOT / "CHANGELOG.md"
CHANGELOG_JSON = FRONTEND_DIR / "data/changelog.json"

LOCAL_CRATES = ["toiler-desktop", "toiler-poc", "toiler-server", "toiler-session"]

# `1.0.4` / `1.0.4-alpha.1` / `v1.0.4` 都认，但写回时统一去掉前导 `v`。
SEMVER = re.compile(r"^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$")

# Cargo.toml 里只改 `[package]` 段的那一行；`name` 紧挨着它，用作锚点避免误伤
# `[dependencies]` 里可能的 `version = ...`。
CARGO_VERSION = re.compile(
    r'(?m)^(name = "toiler-[a-z]+"\nversion = ")([^"]+)(")$'
)

# tauri.conf.json 走文本替换而非 JSON 重排：这份文件里的 `capabilities` 数组是
# 单行手写格式，`json.dumps` 会把它摊成多行，产生无关 diff。
TAURI_VERSION = re.compile(r'(?m)^(  "version": ")([^"]+)(",)$')


class Fail(Exception):
    """可预期的失败：打印一行中文提示后以退出码 1 结束。"""


# --------------------------------------------------------------------------
# 读取
# --------------------------------------------------------------------------


def read_cargo_versions() -> dict[str, str]:
    versions = {}
    for path in CARGO_TOMLS:
        text = path.read_text(encoding="utf-8")
        found = CARGO_VERSION.findall(text)
        if len(found) != 1:
            raise Fail(f"{rel(path)} 里找到 {len(found)} 处 `[package]` 版本行，应为 1 处")
        versions[rel(path)] = found[0][1]
    return versions


def read_tauri_version() -> str:
    text = BASE_FILE.read_text(encoding="utf-8")
    found = TAURI_VERSION.findall(text)
    if len(found) != 1:
        raise Fail(f"{rel(BASE_FILE)} 里找到 {len(found)} 处顶层 version，应为 1 处")
    return found[0][1]


def read_npm_versions() -> dict[str, str]:
    versions = {}
    pkg = json.loads(PACKAGE_JSON.read_text(encoding="utf-8"))
    versions[rel(PACKAGE_JSON)] = pkg.get("version", "")
    lock = json.loads(PACKAGE_LOCK.read_text(encoding="utf-8"))
    versions[rel(PACKAGE_LOCK) + ":顶层"] = lock.get("version", "")
    versions[rel(PACKAGE_LOCK) + ':packages[""]'] = lock.get("packages", {}).get("", {}).get("version", "")
    return versions


def read_lock_versions() -> dict[str, str]:
    if not CARGO_LOCK.exists():
        return {}
    text = CARGO_LOCK.read_text(encoding="utf-8")
    versions = {}
    for name in LOCAL_CRATES:
        found = re.findall(r'(?m)^name = "' + re.escape(name) + r'"\nversion = "([^"]+)"$', text)
        if len(found) != 1:
            raise Fail(f"Cargo.lock 里 {name} 找到 {len(found)} 个包条目，应为 1 个")
        versions[name] = found[0]
    return versions


def read_changelog_versions() -> list[str]:
    text = CHANGELOG_MD.read_text(encoding="utf-8")
    return re.findall(r"(?m)^## \[([^\]]+)\]", text)


def collect_versions() -> dict[str, str]:
    """所有待同步位置的当前版本，键是给人看的说明。"""
    all_versions = {"tauri.conf.json（基数）": read_tauri_version()}
    all_versions.update(read_cargo_versions())
    all_versions.update(read_npm_versions())
    for name, version in read_lock_versions().items():
        all_versions[f"Cargo.lock:{name}"] = version
    return all_versions


def rel(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


# --------------------------------------------------------------------------
# 版本号运算
# --------------------------------------------------------------------------


def parse_semver(raw: str) -> tuple[int, int, int, str]:
    match = SEMVER.match(raw.strip())
    if not match:
        raise Fail(f"`{raw}` 不是合法的 semver（期望 `1.2.3` 或 `1.2.3-alpha.1`）")
    return (
        int(match.group(1)),
        int(match.group(2)),
        int(match.group(3)),
        match.group(4) or "",
    )


def next_version(current: str, level: str) -> tuple[str, str]:
    """返回 (新版本, 说明)。"""
    major, minor, patch, prerelease = parse_semver(current)
    if level == "patch":
        if prerelease:
            # 预发布版的下一个版本不是 patch+1，而是「把预发布转正」。
            return f"{major}.{minor}.{patch}", f"预发布转正：{current} → {major}.{minor}.{patch}"
        return f"{major}.{minor}.{patch + 1}", f"patch 递增：{current} → {major}.{minor}.{patch + 1}"
    if level == "minor":
        return f"{major}.{minor + 1}.0", f"minor 递增：{current} → {major}.{minor + 1}.0"
    if level == "major":
        return f"{major + 1}.0.0", f"major 递增：{current} → {major + 1}.0.0"
    raise Fail(f"未知的递增级别：{level}")


# --------------------------------------------------------------------------
# 发布提交范围
# --------------------------------------------------------------------------


def git_output(*args: str) -> str:
    try:
        result = subprocess.run(
            ["git", *args], cwd=ROOT, capture_output=True, text=True, encoding="utf-8", timeout=60
        )
    except subprocess.TimeoutExpired as exc:
        raise Fail("Git 操作超时，已停止升级，请检查仓库状态") from exc
    except OSError as exc:
        raise Fail(f"无法读取 Git 历史：{exc}") from exc
    if result.returncode != 0:
        raise Fail(f"读取 Git 历史失败：{result.stderr.strip() or result.stdout.strip()}")
    return result.stdout.strip()


def release_commits(since: str | None = None) -> list[tuple[str, str]]:
    """锁定 HEAD 后读取整个提交图；不按路径或 first-parent 过滤，避免漏掉合并内容。"""
    if git_output("rev-parse", "--is-shallow-repository") == "true":
        raise Fail("浅克隆无法保证发布历史完整，请先获取完整历史和标签后重试")
    head = git_output("rev-parse", "HEAD")
    tags = [
        tag for tag in git_output("tag", "--merged", head).splitlines()
        if tag.startswith("v") and SEMVER.fullmatch(tag)
    ]
    if since is not None:
        if since not in tags:
            raise Fail(f"`{since}` 不是当前 HEAD 可达的 v<semver> 发布标签")
        base = since
    elif tags:
        # describe 按提交图距离找最近标签，而不是误用其它分支或版本号最大的标签。
        base = git_output(
            "describe", "--tags", "--abbrev=0", *(f"--match={tag}" for tag in tags), head
        )
    else:
        base = None

    if base:
        base_commit = git_output("rev-parse", f"refs/tags/{base}^{{commit}}")
        revision = f"{base_commit}..{head}"
        print(f"变更记录提交范围：{base}..{head[:12]}（不含起始标签）")
    else:
        revision = head
        print(f"变更记录提交范围：完整历史..{head[:12]}（未找到发布标签）")
    output = git_output("log", "--reverse", "--format=%H%x00%B%x00", revision, "--")
    fields = output.split("\0")
    commits = [
        (fields[i].strip(), fields[i + 1].strip())
        for i in range(0, len(fields) - 1, 2)
    ]
    print(f"共收录 {len(commits)} 个提交（含合并提交及其分支提交）。")
    if not commits:
        print("⚠️  范围内没有新提交，仅生成版本号统一条目。")
    if git_output("status", "--porcelain"):
        print("⚠️  工作区有未提交改动，不会收录到提交记录；请先提交发布内容再生成最终记录。")
    return commits


def changelog_sections(new: str, since: str | None = None) -> list[dict]:
    groups: dict[str, list[str]] = {"Added": [], "Changed": [], "Fixed": []}
    for commit, message in release_commits(since):
        lines = message.splitlines()
        subject = lines[0] if lines else "（无提交说明）"
        match = re.match(r"([a-z]+)(?:\([^)]*\))?!?:\s*", subject)
        kind = match.group(1) if match else ""
        title = {"feat": "Added", "fix": "Fixed"}.get(kind, "Changed")
        # 保留正文细节和哈希，便于人工润色时逐条核对，不能只取最近一次修改。
        details = "；".join(line.strip() for line in lines[1:] if line.strip())
        item = f"{subject}（`{commit[:12]}`）"
        if details:
            item += f"：{details}"
        groups[title].append(item)
    groups["Changed"].append(f"应用、npm、Tauri 和 Cargo 版本号统一为 `{new}`。")
    return [
        {"title": title, "kind": title.lower(), "items": items}
        for title, items in groups.items() if items
    ]


# --------------------------------------------------------------------------
# 改写
# --------------------------------------------------------------------------


def plan_edits(
    new: str, *, with_changelog: bool, since: str | None = None
) -> list[tuple[Path, str]]:
    """生成 (路径, 新内容) 列表。纯计算，不落盘。"""
    edits: list[tuple[Path, str]] = []

    for path in CARGO_TOMLS:
        text = path.read_text(encoding="utf-8")
        updated, count = CARGO_VERSION.subn(rf"\g<1>{new}\g<3>", text)
        if count != 1:
            raise Fail(f"{rel(path)} 版本行替换了 {count} 处，应为 1 处")
        edits.append((path, updated))

    text = BASE_FILE.read_text(encoding="utf-8")
    updated, count = TAURI_VERSION.subn(rf"\g<1>{new}\g<3>", text)
    if count != 1:
        raise Fail(f"{rel(BASE_FILE)} 版本替换了 {count} 处，应为 1 处")
    edits.append((BASE_FILE, updated))

    # 这两份 JSON 用 json 往返是逐字节安全的（缩进 2 + 末尾换行），所以走结构化改写，
    # 比正则可靠。tauri.conf.json 不在此列，原因见 TAURI_VERSION 的注释。
    pkg = json.loads(PACKAGE_JSON.read_text(encoding="utf-8"))
    pkg["version"] = new
    edits.append((PACKAGE_JSON, dump_json(pkg)))

    lock = json.loads(PACKAGE_LOCK.read_text(encoding="utf-8"))
    lock["version"] = new
    if "" in lock.get("packages", {}):
        lock["packages"][""]["version"] = new
    edits.append((PACKAGE_LOCK, dump_json(lock)))

    if with_changelog:
        edits.extend(plan_changelog_edits(new, since=since))

    return edits


def dump_json(data: object) -> str:
    return json.dumps(data, indent=2, ensure_ascii=False) + "\n"


def plan_changelog_edits(new: str, *, since: str | None = None) -> list[tuple[Path, str]]:
    today = datetime.date.today().isoformat()

    md = CHANGELOG_MD.read_text(encoding="utf-8")
    if re.search(r"(?m)^## \[" + re.escape(new) + r"\]", md):
        raise Fail(f"CHANGELOG.md 里已存在 `## [{new}]` 段，换个版本号或手工处理")
    anchor = re.search(r"(?m)^## \[", md)
    if not anchor:
        raise Fail("CHANGELOG.md 里找不到任何 `## [版本]` 段，无法确定插入位置")
    releases = json.loads(CHANGELOG_JSON.read_text(encoding="utf-8"))
    if any(r.get("version") == new for r in releases):
        raise Fail(f"data/changelog.json 里已存在 {new} 条目")

    sections = changelog_sections(new, since)
    block = f"## [{new}] - {today}\n\n"
    for section in sections:
        block += f"### {section['title']}\n\n"
        block += "".join(f"- {item}\n" for item in section["items"]) + "\n"
    print("变更记录预览：")
    print(block)
    md = md[: anchor.start()] + block + md[anchor.start() :]
    releases.insert(
        0,
        {"version": new, "date": today, "sections": sections},
    )

    return [(CHANGELOG_MD, md), (CHANGELOG_JSON, dump_json(releases))]


def update_cargo_lock(new: str) -> None:
    """用 cargo 更新 Cargo.lock；cargo 不可用或离线失败时退回文本改写。"""
    cargo = shutil.which("cargo")
    if cargo:
        args = [cargo, "update", "--offline"]
        for name in LOCAL_CRATES:
            args.extend(["-p", name])
        try:
            result = subprocess.run(args, cwd=ROOT, capture_output=True, text=True, timeout=120)
        except subprocess.TimeoutExpired as exc:
            raise Fail("Cargo.lock 更新超时，请检查是否有其它 Cargo 操作占用锁") from exc
        if result.returncode == 0:
            print(f"  Cargo.lock：cargo update 已把 {len(LOCAL_CRATES)} 个本地包锁定到 {new}")
            return
        print("  ⚠️  cargo update 失败，退回文本改写 Cargo.lock：")
        print(indent(result.stderr.strip() or result.stdout.strip()))
    else:
        print("  ⚠️  PATH 里没有 cargo，退回文本改写 Cargo.lock")

    if not CARGO_LOCK.exists():
        raise Fail("Cargo.lock 不存在，先跑一次 cargo build")
    text = CARGO_LOCK.read_text(encoding="utf-8")
    for name in LOCAL_CRATES:
        pattern = re.compile(r'(?m)^(name = "' + re.escape(name) + r'"\nversion = ")([^"]+)(")$')
        text, count = pattern.subn(rf"\g<1>{new}\g<3>", text)
        if count != 1:
            raise Fail(f"Cargo.lock 里 {name} 版本行替换了 {count} 处，应为 1 处")
    CARGO_LOCK.write_text(text, encoding="utf-8")
    print(f"  Cargo.lock：已文本改写 {len(LOCAL_CRATES)} 个本地包到 {new}")


# --------------------------------------------------------------------------
# 桌面发布桥（CLI 本身仍不提交、不打标签）
# --------------------------------------------------------------------------


def desktop_release(
    level: str, *, dry_run: bool, expected_head: str | None = None,
    expected_version: str | None = None, expected_plan: str | None = None,
) -> dict:
    """仅由桌面壳调用；复用同一份规划和锁文件逻辑，确认后提交并创建普通标签。"""
    if level not in ("patch", "minor", "major"):
        raise Fail("未知的升级级别")
    if Path(git_output("rev-parse", "--show-toplevel")).resolve() != ROOT.resolve():
        raise Fail("版本升级必须在项目仓库根目录执行")
    if not git_output("symbolic-ref", "--quiet", "HEAD"):
        raise Fail("请先切换到本地分支再升级版本")
    git_dir = Path(git_output("rev-parse", "--absolute-git-dir"))
    if any((git_dir / name).exists() for name in ("MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-merge", "rebase-apply", "sequencer")):
        raise Fail("请先完成或中止正在进行的合并、变基或拣选操作")
    if git_output("status", "--porcelain", "--untracked-files=all"):
        raise Fail("请先提交或贮藏所有未提交改动，再升级版本；不会混入其它修改")

    paths = [*CARGO_TOMLS, BASE_FILE, PACKAGE_JSON, PACKAGE_LOCK, CHANGELOG_MD, CHANGELOG_JSON, CARGO_LOCK]
    # 防止项目中的符号链接把版本写入引到仓库外；桌面桥只支持脚本约定的项目结构。
    for path in [ROOT / "Cargo.toml", *paths]:
        try:
            path.resolve().relative_to(ROOT.resolve())
        except ValueError as exc:
            raise Fail(f"版本文件指向仓库外：{rel(path)}") from exc
        if not path.is_file():
            raise Fail(f"项目缺少版本文件：{rel(path)}，只支持与 bump-version.py 兼容的项目")
    files = [path.relative_to(ROOT).as_posix() for path in paths]
    git_output("ls-files", "--error-unmatch", "--", *files)
    head = git_output("rev-parse", "HEAD")
    current = read_tauri_version()
    new, _ = next_version(current, level)
    tag = f"v{new}"
    if git_output("tag", "--list", tag):
        raise Fail(f"标签 {tag} 已存在，不能重复发布")
    git_output("var", "GIT_AUTHOR_IDENT")
    git_output("var", "GIT_COMMITTER_IDENT")
    edits = plan_edits(new, with_changelog=True)
    plan_id = hashlib.sha256(
        json.dumps([(rel(path), content) for path, content in edits], ensure_ascii=False).encode("utf-8")
    ).hexdigest()
    result = {"current": current, "version": new, "tag": tag, "head": head, "planId": plan_id, "files": files}
    if dry_run:
        return result
    if (expected_head, expected_version, expected_plan) != (head, new, plan_id):
        raise Fail("仓库或升级预览已经变化，请重新预览并确认")

    try:
        for path, content in edits:
            path.write_text(content, encoding="utf-8")
        update_cargo_lock(new)
        if any(version != new for version in collect_versions().values()):
            raise Fail("升级后版本号未全部对齐，已停止提交")
        contents = {path: path.read_bytes() for path in paths}
        changed = set(git_output("diff", "--name-only", "-z", "HEAD", "--").split("\0")) - {""}
        if not changed or not changed.issubset(set(files)):
            raise Fail("升级期间出现预期之外的文件改动，已停止提交")
        if git_output("rev-parse", "HEAD") != head:
            raise Fail("提交历史在升级期间发生变化，已停止提交")
        # --only 只提交版本文件；即使外部工具并发暂存其它文件，也不把它们混入版本提交。
        git_output(
            "-c", "commit.gpgsign=false", "commit", "--only", "-m",
            f"chore(release): 版本号升级至 {new}", "--", *files,
        )
        commit = git_output("rev-parse", "HEAD")
        if git_output("rev-parse", "HEAD^") != head:
            raise Fail("提交历史在升级期间发生变化，已停止创建标签")
        committed = set(git_output("diff-tree", "--no-commit-id", "--name-only", "-r", "-z", "HEAD").split("\0")) - {""}
        if committed != changed or any(path.read_bytes() != content for path, content in contents.items()) or git_output("status", "--porcelain", "--untracked-files=all"):
            raise Fail("提交钩子或外部工具修改了仓库，请检查版本提交；尚未创建标签")
        # 显式禁用 tag.gpgsign，普通标签不能因用户的全局配置变成签名操作。
        git_output("-c", "tag.gpgsign=false", "tag", "--no-sign", tag, commit)
        result["commit"] = commit
        print(f"已创建版本提交 {commit[:12]} 与普通标签 {tag}；未推送到远端。")
        return result
    except Exception as exc:
        raise Fail(f"{exc}\n升级产生的文件或提交已保留，未自动回滚或推送，请检查仓库后再继续。") from exc


# --------------------------------------------------------------------------
# 校验与提示
# --------------------------------------------------------------------------


def cmd_check() -> int:
    versions = collect_versions()
    base = versions["tauri.conf.json（基数）"]
    mismatched = {k: v for k, v in versions.items() if v != base}

    print(f"基数（tauri.conf.json）：{base}")
    for where, version in versions.items():
        mark = "ok  " if version == base else "差异"
        print(f"  {mark} {where}: {version}")

    if mismatched:
        print("\n版本号不一致，跑 tools/bump-version.py 对齐，或手工修上面标为「差异」的项。")
        return 1
    print("\n全部一致。")
    return 0


def stale_references(old: str) -> list[str]:
    """改动后仍残留旧版本号的地方（排除历史变更记录里的正常引用）。"""
    hints = []
    md = CHANGELOG_MD.read_text(encoding="utf-8")
    if re.search(r"(?m)^## \[" + re.escape(old) + r"\]", md):
        hints.append(f"{rel(CHANGELOG_MD)}：作为历史条目保留（正常）")
    print_versions = read_changelog_versions()
    if old in print_versions:
        hints.append(f"{rel(CHANGELOG_JSON)}：作为历史条目保留（正常）")
    return hints


def print_next_steps(new: str, *, dry_run: bool, wrote_changelog: bool) -> None:
    prefix = "（dry-run，未落盘）" if dry_run else ""
    print()
    print(f"下一步{prefix}：")
    if wrote_changelog:
        print("  1. 审核按完整发布提交范围生成的变更记录，可润色文案但应逐条核对避免遗漏：")
        print(f"       {rel(CHANGELOG_MD)} 与 {rel(CHANGELOG_JSON)} 的 {new} 段要一起改")
    else:
        print("  1. 变更记录未动（--no-changelog），自己加段（两份要一致）：")
        print(f"       {rel(CHANGELOG_MD)}：`## [{new}] - <日期>`")
        print(f"       {rel(CHANGELOG_JSON)}：头部的同版本条目（界面读这份）")
    print("  2. 重建前端（版本号由 next.config.ts 注入产物，不重建则二进制里还是旧号）：")
    print("       source \"${NVM_DIR:-$HOME/.nvm}/nvm.sh\" && nvm use 22.23.5")
    print("       cd crates/toiler-desktop/frontend && npm run typecheck && npm test && npm run build")
    print("  3. 验证（工作区根目录，任一失败即停）：")
    print("       export PATH=\"$HOME/.cargo/bin:$PATH\"")
    print("       cargo build --workspace && cargo test --workspace")
    print("       cargo clippy --all-targets && cargo fmt --all -- --check && git diff --check")
    print("  4. 提交并打 tag（tag 必须与 tauri.conf.json 的 version 一致）：")
    print(f"       git add -A && git commit -m \"chore(release): 版本号升级至 {new}\"")
    print(f"       git tag v{new} && git push origin main --tags")


def indent(text: str) -> str:
    return "\n".join("      " + line for line in text.splitlines())


# --------------------------------------------------------------------------
# 入口
# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="bump-version.py",
        description="把 Toiler 的版本号升到下一个版本（或校验各处是否一致）。",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="版本号散落位置与注意事项见本脚本头部的注释。",
    )
    parser.add_argument("version", nargs="?", help="目标版本，如 1.2.0；省略则按 --patch/--minor/--major 递增")
    level = parser.add_mutually_exclusive_group()
    level.add_argument("--patch", action="store_const", const="patch", dest="level", help="patch 递增（默认）")
    level.add_argument("--minor", action="store_const", const="minor", dest="level", help="minor 递增")
    level.add_argument("--major", action="store_const", const="major", dest="level", help="major 递增")
    parser.add_argument("--dry-run", action="store_true", help="只打印将要改什么，不写文件")
    parser.add_argument("--check", action="store_true", help="只校验各文件版本是否一致")
    parser.add_argument("--no-lock", action="store_true", help="跳过 Cargo.lock 更新")
    parser.add_argument("--no-changelog", action="store_true", help="不写变更记录")
    parser.add_argument("--since", metavar="TAG", help="变更记录起始发布标签（默认最近的可达发布标签）")
    # 必须在 parse_args 之前设默认值，否则互斥组会把 dest 默认成 None。
    parser.set_defaults(level="patch")
    args = parser.parse_args(argv)
    if args.since and (args.no_changelog or args.check):
        raise Fail("--since 不能与 --no-changelog 或 --check 一起使用")

    if not (ROOT / "Cargo.toml").exists():
        raise Fail(f"{ROOT} 不像仓库根目录（缺 Cargo.toml）")

    if args.check:
        return cmd_check()

    if args.version and args.level != "patch":
        raise Fail("给了具体版本号就不要再带 --minor/--major")

    current = read_tauri_version()
    if args.version:
        parse_semver(args.version)  # 先校验形状
        new = args.version.lstrip("v")
        note = f"指定版本：{current} → {new}"
    else:
        new, note = next_version(current, args.level)

    if new == current:
        raise Fail(f"新版本与当前版本相同（{current}），无需升级")

    print(f"版本：{note}")
    print()

    # 升级前先对齐：基数与其它文件不一致时，直接改会把差异固化下来。
    versions = collect_versions()
    base = versions["tauri.conf.json（基数）"]
    drifted = {k: v for k, v in versions.items() if v != base}
    if drifted:
        print(f"⚠️  当前版本号本身就不一致（基数 {base}），升级会一并拉平：")
        for where, version in drifted.items():
            print(f"     {where}: {version}")
        print()

    edits = plan_edits(new, with_changelog=not args.no_changelog, since=args.since)

    print("将改动：")
    for path, content in edits:
        old_content = path.read_text(encoding="utf-8")
        if old_content == content:
            print(f"  - {rel(path)}（无变化）")
            continue
        print(f"  - {rel(path)}")
    if not args.no_lock:
        print(f"  - {rel(CARGO_LOCK)}（cargo update）")

    if args.dry_run:
        print_next_steps(new, dry_run=True, wrote_changelog=not args.no_changelog)
        return 0

    print()
    print("落盘：")
    for path, content in edits:
        path.write_text(content, encoding="utf-8")
        print(f"  {rel(path)}")
    if not args.no_lock:
        update_cargo_lock(new)

    # 落盘后复查：任何一处没跟上都立刻报出来，别等到打 tag 才发现。
    versions = collect_versions()
    leftover = {
        k: v
        for k, v in versions.items()
        if v != new and not (args.no_lock and k.startswith("Cargo.lock:"))
    }
    if leftover:
        print()
        print("❌ 升级后仍有位置不是新版本：")
        for where, version in leftover.items():
            print(f"     {where}: {version}")
        if args.no_lock:
            print(f"\n（已跳过 Cargo.lock，记得自己跑一次 cargo update -p ...）")
        return 1

    hints = stale_references(current)
    if hints:
        print()
        print("旧版本号仍然出现在：")
        for hint in hints:
            print(f"  - {hint}")

    print_next_steps(new, dry_run=False, wrote_changelog=not args.no_changelog)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Fail as exc:
        print(f"错误：{exc}", file=sys.stderr)
        sys.exit(1)
