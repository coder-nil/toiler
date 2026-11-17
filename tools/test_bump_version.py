#!/usr/bin/env python3
"""用隔离的 Git 仓库验证发布范围与两份变更记录，不触碰真实版本文件。"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

PROJECT_ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("bump_version", PROJECT_ROOT / "tools/bump-version.py")
assert spec and spec.loader
bump = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bump)


class BumpVersionTests(unittest.TestCase):
    def setUp(self):
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.root = Path(self.stack.enter_context(
            tempfile.TemporaryDirectory(prefix=".bump-version-test-", dir=PROJECT_ROOT)
        ))
        self.stack.enter_context(patch.object(bump, "ROOT", self.root))
        self.output = self.stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
        self.git("-c", "init.templateDir=", "init", "-b", "main")
        self.git("config", "user.name", "版本脚本测试")
        self.git("config", "user.email", "test@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "tag.gpgsign", "false")
        self.initial = self.commit("chore: 初始提交")

    def git(self, *args):
        env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        result = subprocess.run(
            ["git", *args], cwd=self.root, capture_output=True,
            text=True, encoding="utf-8", env=env, check=True,
        )
        return result.stdout.strip()

    def commit(self, subject, body=None):
        args = ["commit", "--allow-empty", "-m", subject]
        if body:
            args.extend(["-m", body])
        self.git(*args)
        return self.git("rev-parse", "HEAD")

    def sections(self, since=None):
        return bump.changelog_sections("1.0.1", since)

    def items(self, sections):
        return [item for section in sections for item in section["items"]]

    def prepare_files(self):
        paths = {
            "BASE_FILE": self.root / "tauri.conf.json",
            "PACKAGE_JSON": self.root / "package.json",
            "PACKAGE_LOCK": self.root / "package-lock.json",
            "CARGO_LOCK": self.root / "Cargo.lock",
            "CHANGELOG_MD": self.root / "CHANGELOG.md",
            "CHANGELOG_JSON": self.root / "changelog.json",
        }
        for name, path in paths.items():
            self.stack.enter_context(patch.object(bump, name, path))
        tomls = [self.root / name / "Cargo.toml" for name in bump.LOCAL_CRATES]
        self.stack.enter_context(patch.object(bump, "CARGO_TOMLS", tomls))
        (self.root / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        for name, path in zip(bump.LOCAL_CRATES, tomls):
            path.parent.mkdir()
            path.write_text(f'[package]\nname = "{name}"\nversion = "1.0.0"\n', encoding="utf-8")
        bump.BASE_FILE.write_text('{\n  "version": "1.0.0",\n  "app": {}\n}\n', encoding="utf-8")
        bump.PACKAGE_JSON.write_text('{"version": "1.0.0"}\n', encoding="utf-8")
        bump.PACKAGE_LOCK.write_text(json.dumps({
            "version": "1.0.0", "packages": {"": {"version": "1.0.0"}}
        }), encoding="utf-8")
        bump.CARGO_LOCK.write_text("\n".join(
            f'[[package]]\nname = "{name}"\nversion = "1.0.0"\n'
            for name in bump.LOCAL_CRATES
        ), encoding="utf-8")
        self.old_md = "# 变更记录\n\n## [1.0.0] - 2020-01-01\n\n### Added\n\n- 历史 Markdown。\n"
        self.old_releases = [{
            "version": "1.0.0", "date": "2020-01-01",
            "sections": [{"title": "Added", "kind": "added", "items": ["独立的历史 JSON。"]}],
        }]
        bump.CHANGELOG_MD.write_text(self.old_md, encoding="utf-8")
        bump.CHANGELOG_JSON.write_text(bump.dump_json(self.old_releases), encoding="utf-8")
        self.git("add", ".")
        self.commit("chore: 发布文件")
        self.git("tag", "v1.0.0")
        return list(paths.values()) + tomls + [self.root / "Cargo.toml"]

    def test_all_commits_and_bodies_are_grouped_and_traceable(self):
        self.git("tag", "v1.0.0")
        added = self.commit("feat(server)!: 新增接口", "- 第一项\n- 第二项")
        fixed = self.commit("fix(desktop): 修复窗口")
        docs = self.commit("docs: 补充文档")
        sections = self.sections()
        groups = {section["title"]: section["items"] for section in sections}
        self.assertIn(added[:12], groups["Added"][0])
        self.assertIn("第一项；- 第二项", groups["Added"][0])
        self.assertIn(fixed[:12], groups["Fixed"][0])
        self.assertIn(docs[:12], groups["Changed"][0])
        self.assertEqual(len(self.items(sections)), 4)
        self.assertNotIn(self.initial[:12], "\n".join(self.items(sections)))

    def test_merged_branch_and_merge_commit_are_included(self):
        self.git("tag", "v1.0.0")
        self.git("checkout", "-b", "feature")
        feature = self.commit("feat: 分支功能")
        self.git("checkout", "main")
        main = self.commit("fix: 主线修复")
        self.git("merge", "--no-ff", "feature", "-m", "合并功能分支")
        merge = self.git("rev-parse", "HEAD")
        actual = {sha for sha, _ in bump.release_commits()}
        self.assertEqual(actual, {feature, main, merge})

    def test_nearest_release_ignores_unmerged_and_nonrelease_tags(self):
        self.git("tag", "v1.0.0")
        self.git("checkout", "-b", "other")
        self.commit("feat: 未合并功能")
        self.git("tag", "v99.0.0")
        self.git("checkout", "main")
        self.commit("fix: 已发布修复")
        self.git("tag", "-a", "v1.0.1", "-m", "发布")
        expected = self.commit("feat: 本次功能")
        self.git("tag", "v2.0.0-not_a_release")
        self.assertEqual([sha for sha, _ in bump.release_commits()], [expected])
        self.assertIn("v1.0.1..", self.output.getvalue())

    def test_explicit_since_overrides_nearest_tag(self):
        self.git("tag", "v1.0.0")
        first = self.commit("feat: 第一项")
        self.git("tag", "v1.0.1")
        second = self.commit("fix: 第二项")
        self.assertEqual([sha for sha, _ in bump.release_commits("v1.0.0")], [first, second])

    def test_missing_or_unreachable_since_is_rejected(self):
        self.git("checkout", "-b", "other")
        self.commit("feat: 其它分支")
        self.git("tag", "v9.0.0")
        self.git("checkout", "main")
        for tag in ("missing", "v9.0.0", "--all"):
            with self.subTest(tag=tag), self.assertRaises(bump.Fail):
                bump.release_commits(tag)

    def test_no_tag_includes_entire_history(self):
        second = self.commit("新增普通提交")
        self.assertEqual([sha for sha, _ in bump.release_commits()], [self.initial, second])

    def test_empty_range_keeps_version_item_and_warns(self):
        self.git("tag", "v1.0.0")
        items = self.items(self.sections())
        self.assertEqual(items, ["应用、npm、Tauri 和 Cargo 版本号统一为 `1.0.1`。"])
        self.assertIn("没有新提交", self.output.getvalue())

    def test_dirty_worktree_is_not_presented_as_committed(self):
        self.git("tag", "v1.0.0")
        (self.root / "uncommitted.txt").write_text("未提交功能", encoding="utf-8")
        self.assertEqual(bump.release_commits(), [])
        self.assertIn("未提交改动", self.output.getvalue())

    def test_shallow_history_is_rejected(self):
        self.commit("fix: 后续修复")
        clone = self.root / "shallow"
        self.git("clone", "--depth=1", self.root.as_uri(), str(clone))
        with patch.object(bump, "ROOT", clone), self.assertRaisesRegex(bump.Fail, "浅克隆"):
            bump.release_commits()

    def test_both_formats_match_and_preserve_independent_history(self):
        self.prepare_files()
        commit = self.commit("feat: 发布功能", "完整正文")
        edits = dict(bump.plan_changelog_edits("1.0.1"))
        md = edits[bump.CHANGELOG_MD]
        releases = json.loads(edits[bump.CHANGELOG_JSON])
        self.assertTrue(md.endswith("## " + self.old_md.split("## ", 1)[1]))
        self.assertEqual(releases[1:], self.old_releases)
        self.assertEqual(md.count(f"`{commit[:12]}`"), 1)
        for section in releases[0]["sections"]:
            self.assertIn(f"### {section['title']}\n", md)
            for item in section["items"]:
                self.assertIn(f"- {item}\n", md)
        self.assertEqual(bump.CHANGELOG_MD.read_text(encoding="utf-8"), self.old_md)

    def test_duplicate_versions_are_rejected_in_either_format(self):
        self.prepare_files()
        with self.assertRaises(bump.Fail):
            bump.plan_changelog_edits("1.0.0")
        releases = [{"version": "1.0.1"}] + self.old_releases
        bump.CHANGELOG_JSON.write_text(bump.dump_json(releases), encoding="utf-8")
        with self.assertRaises(bump.Fail):
            bump.plan_changelog_edits("1.0.1")

    def test_dry_run_previews_all_commits_without_writing(self):
        files = self.prepare_files()
        commit = self.commit("fix: 预览修复")
        before = {path: path.read_bytes() for path in files}
        self.assertEqual(bump.main(["--dry-run"]), 0)
        self.assertEqual({path: path.read_bytes() for path in files}, before)
        self.assertIn(commit[:12], self.output.getvalue())
        self.assertIn("预览修复", self.output.getvalue())

    def test_actual_bump_writes_matching_changelogs_without_git_changes(self):
        self.prepare_files()
        commit = self.commit("feat: 本次发布功能")
        head = self.git("rev-parse", "HEAD")
        tags = self.git("tag")
        self.assertEqual(bump.main(["--no-lock"]), 0)
        self.assertEqual(bump.read_tauri_version(), "1.0.1")
        md = bump.CHANGELOG_MD.read_text(encoding="utf-8")
        releases = json.loads(bump.CHANGELOG_JSON.read_text(encoding="utf-8"))
        self.assertIn(commit[:12], md)
        for item in self.items(releases[0]["sections"]):
            self.assertIn(f"- {item}\n", md)
        self.assertEqual(self.git("rev-parse", "HEAD"), head)
        self.assertEqual(self.git("tag"), tags)

    def test_no_changelog_and_check_do_not_require_history(self):
        self.prepare_files()
        with patch.object(bump, "release_commits", side_effect=AssertionError("不应读取历史")):
            self.assertEqual(bump.main(["--dry-run", "--no-changelog"]), 0)
            self.assertEqual(bump.main(["--check"]), 0)

    def test_since_conflicts_with_options_that_skip_changelog(self):
        for option in ("--no-changelog", "--check"):
            with self.subTest(option=option), self.assertRaises(bump.Fail):
                bump.main(["--since", "v1.0.0", option])


if __name__ == "__main__":
    unittest.main()
