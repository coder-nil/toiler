//! 版本移植规则的回归用例。
//!
//! 重点是**与 `tools/bump-version.py` 语义一致**：桌面壳走 Rust，开发者 CLI 走
//! Python，两边算出的下一版本 / 变更记录分类必须相同，否则「界面上看到的预览」
//! 和「脚本算出来的」会不一致。

use super::*;

fn commit(hash: &str, subject: &str, details: &str) -> ReleaseCommit {
    ReleaseCommit {
        hash: hash.into(),
        subject: subject.into(),
        details: details.into(),
    }
}

// ---------------------------------------------------------------- semver

#[test]
fn parses_plain_and_prerelease_and_v_prefix() {
    let v = Semver::parse("1.2.3").unwrap();
    assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));
    assert_eq!(v.pre, "");

    let v = Semver::parse("v1.0.4").unwrap();
    assert_eq!(v.render(), "1.0.4", "前导 v 要去掉");

    let v = Semver::parse("1.0.4-alpha.1").unwrap();
    assert_eq!(v.pre, "alpha.1");
    assert_eq!(v.render(), "1.0.4-alpha.1");
}

#[test]
fn rejects_malformed_versions() {
    for bad in ["1.2", "1.2.3.4", "abc", "", "1.2.x", "v", "1.2.3-"] {
        assert!(Semver::parse(bad).is_err(), "应拒绝：{bad}");
    }
}

/// 预发布转正是最容易搞错的一条：`1.2.3-beta.1` 的 patch 不是 `1.2.4`。
/// `tools/bump-version.py` 的 `next_version` 同样如此。
#[test]
fn patch_on_prerelease_promotes_instead_of_incrementing() {
    let current = Semver::parse("1.2.3-beta.1").unwrap();
    let (next, note) = next_version(&current, Level::Patch);
    assert_eq!(next.render(), "1.2.3");
    assert!(note.contains("预发布转正"), "{note}");
}

#[test]
fn increments_match_python_rules() {
    let cases = [
        ("1.0.4", Level::Patch, "1.0.5"),
        ("1.0.4", Level::Minor, "1.1.0"),
        ("1.0.4", Level::Major, "2.0.0"),
        // minor/major 会清掉预发布后缀。
        ("1.2.3-beta.1", Level::Minor, "1.3.0"),
        ("1.2.3-beta.1", Level::Major, "2.0.0"),
        ("0.0.1", Level::Patch, "0.0.2"),
    ];
    for (current, level, want) in cases {
        let (next, _) = next_version(&Semver::parse(current).unwrap(), level);
        assert_eq!(next.render(), want, "{current} + {level:?}");
    }
}

// ------------------------------------------------------- 提交分类

#[test]
fn classifies_conventional_commits() {
    assert_eq!(conventional_kind("feat(git): 加功能"), "feat");
    assert_eq!(conventional_kind("fix: 修"), "fix");
    assert_eq!(conventional_kind("feat!: 破坏性"), "feat");
    assert_eq!(conventional_kind("feat(scope)!: x"), "feat");
    assert_eq!(conventional_kind("chore: x"), "chore");
    // 无 conventional 前缀 / 合并提交 / scope 未闭合。
    assert_eq!(conventional_kind("Merge branch 'x'"), "");
    assert_eq!(conventional_kind("随便写的主题"), "");
    assert_eq!(conventional_kind("feat(broken: x"), "");
    assert_eq!(conventional_kind("FEAT: 大写不算"), "");
}

#[test]
fn sections_split_feat_fix_and_everything_else() {
    let commits = [
        commit("aaaaaaaaaaaabbbb", "feat(git): 加面板", "细节一；细节二"),
        commit("ccccccccccccdddd", "fix(server): 修崩溃", ""),
        commit("eeeeeeeeeeeeffff", "chore: 杂务", ""),
        commit("1111111111112222", "Merge branch 'x'", ""),
    ];
    let sections = changelog_sections(&commits, "1.0.5");

    let by_title = |name: &str| {
        sections
            .iter()
            .find(|s| s.title == name)
            .map(|s| s.items.clone())
            .unwrap_or_default()
    };

    let added = by_title("Added");
    assert_eq!(added.len(), 1);
    assert!(
        added[0].starts_with("feat(git): 加面板（`aaaaaaaaaaaa`）：细节一；细节二"),
        "{added:?}"
    );

    let fixed = by_title("Fixed");
    assert_eq!(fixed.len(), 1);
    assert_eq!(fixed[0], "fix(server): 修崩溃（`cccccccccccc`）");

    // chore 与合并没有 conventional 前缀 → Changed，且不能丢。
    let changed = by_title("Changed");
    assert_eq!(changed.len(), 3);
    assert!(changed.iter().any(|i| i.contains("chore: 杂务")));
    assert!(changed.iter().any(|i| i.contains("Merge branch")));
    // 末尾那条版本统一说明自动加。
    assert!(
        changed.last().unwrap().contains("统一为 `1.0.5`"),
        "{changed:?}"
    );
}

#[test]
fn sections_skip_empty_groups_and_keep_order() {
    let sections = changelog_sections(&[commit("aaaaaaaaaaaa", "fix: x", "")], "2.0.0");
    let titles: Vec<&str> = sections.iter().map(|s| s.title).collect();
    // Added 为空被跳过，但 Changed（版本统一说明）恒存在，Fixed 有内容。
    assert_eq!(titles, vec!["Changed", "Fixed"]);
    assert_eq!(sections[0].kind, "changed");
    assert_eq!(sections[1].kind, "fixed");
}

#[test]
fn no_commits_still_records_version_unification() {
    let sections = changelog_sections(&[], "1.0.8");
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].title, "Changed");
    assert!(sections[0].items[0].contains("统一为 `1.0.8`"));
}

// ------------------------------------------------------ Cargo.lock

const LOCK_SAMPLE: &str = r#"version = 3

[[package]]
name = "anyhow"
version = "1.0.86"

[[package]]
name = "toiler-server"
version = "1.0.7"
dependencies = ["axum"]

[[package]]
name = "toiler-session"
version = "1.0.7"

[[package]]
name = "toiler-desktop"
version = "1.0.7"

[[package]]
name = "toiler-poc"
version = "1.0.7"
"#;

#[test]
fn rewrites_only_local_crates_in_lock() {
    let out = rewrite_cargo_lock(LOCK_SAMPLE, "1.0.8").unwrap();
    assert!(
        out.contains("name = \"anyhow\"\nversion = \"1.0.86\""),
        "第三方包不能动"
    );
    assert!(out.contains("name = \"toiler-server\"\nversion = \"1.0.8\""));
    assert!(out.contains("name = \"toiler-session\"\nversion = \"1.0.8\""));
    assert!(out.contains("name = \"toiler-desktop\"\nversion = \"1.0.8\""));
    assert!(out.contains("name = \"toiler-poc\"\nversion = \"1.0.8\""));
    assert!(!out.contains("1.0.7"), "不该残留旧版本：\n{out}");
}

#[test]
fn lock_rewrite_rejects_missing_or_duplicated_crate() {
    // 缺一个本地包（每个都必须恰好 1 处）。
    let missing = LOCK_SAMPLE.replace(
        "[[package]]\nname = \"toiler-poc\"\nversion = \"1.0.7\"",
        "",
    );
    let err = rewrite_cargo_lock(&missing, "1.0.8").unwrap_err().0;
    assert!(err.contains("toiler-poc"), "{err}");

    // 同名包出现两次。
    let duplicate =
        format!("{LOCK_SAMPLE}\n[[package]]\nname = \"toiler-poc\"\nversion = \"1.0.7\"\n");
    let err = rewrite_cargo_lock(&duplicate, "1.0.8").unwrap_err().0;
    assert!(err.contains("2 处"), "{err}");
}

// ------------------------------------------------- 文本改写（不能重排）

/// `tauri.conf.json` 的 `capabilities` 是单行手写格式，JSON 往返会摊成多行。
/// 这里断言改写后**除 version 那一行外逐字节相同**。
#[test]
fn tauri_rewrite_touches_only_the_version_line() {
    let original = "{\n  \"productName\": \"Toiler\",\n  \"version\": \"1.0.7\",\n  \"bundle\": {\n    \"capabilities\": [\"core:default\", \"shell:allow-open\"]\n  }\n}\n";
    let out = rewrite_tauri_version(original, "1.0.8").unwrap();
    let expected = original.replace("\"version\": \"1.0.7\"", "\"version\": \"1.0.8\"");
    assert_eq!(out, expected);
    // 单行数组没有被摊开。
    assert!(out.contains("\"capabilities\": [\"core:default\", \"shell:allow-open\"]"));
}

#[test]
fn tauri_rewrite_requires_exactly_one_version_line() {
    let none = "{\n  \"productName\": \"x\"\n}\n";
    assert!(rewrite_tauri_version(none, "1.0.8").is_err());

    // 缩进更深的不算顶层（bundle.version 之类）。
    let nested =
        "{\n  \"version\": \"1.0.7\",\n  \"bundle\": {\n    \"version\": \"9.9.9\",\n  }\n}\n";
    let out = rewrite_tauri_version(nested, "1.0.8").unwrap();
    assert!(out.contains("  \"version\": \"1.0.8\""));
    assert!(out.contains("\"version\": \"9.9.9\""), "嵌套版本不能被改");
}

#[test]
fn cargo_toml_rewrite_anchors_on_package_name() {
    let original = "[package]\nname = \"toiler-server\"\nversion = \"1.0.7\"\nedition = \"2021\"\n\n[dependencies]\nanyhow = { version = \"1\" }\n";
    let out = rewrite_cargo_toml(original, "1.0.8").unwrap();
    assert!(out.contains("name = \"toiler-server\"\nversion = \"1.0.8\""));
    // 依赖里的 version 不能被误伤。
    assert!(out.contains("anyhow = { version = \"1\" }"));
}

#[test]
fn cargo_toml_without_matching_package_fails() {
    let other = "[package]\nname = \"unrelated\"\nversion = \"1.0.7\"\n";
    assert!(rewrite_cargo_toml(other, "1.0.8").is_err());
}

// ------------------------------------------------------------- 其他

#[test]
fn level_serializes_lowercase_for_the_wire() {
    assert_eq!(serde_json::to_string(&Level::Patch).unwrap(), "\"patch\"");
    assert_eq!(
        serde_json::from_str::<Level>("\"major\"").unwrap(),
        Level::Major
    );
    assert!(serde_json::from_str::<Level>("\"bogus\"").is_err());
}

#[test]
fn today_has_iso_shape() {
    let today = today_local();
    assert_eq!(today.len(), 10, "{today}");
    let (y, rest) = today.split_once('-').unwrap();
    assert_eq!(y.len(), 4);
    assert_eq!(rest.len(), 5);
}

#[test]
fn json_dump_matches_repo_formatting() {
    // 两份 package 文件都是 2 空格缩进 + 末尾换行；不一致会产生无关 diff。
    let text = dump_json(&json!({"version": "1.0.8", "nested": {"a": 1}}));
    assert!(text.contains("  \"version\": \"1.0.8\""));
    assert!(text.ends_with("}\n"));
}
