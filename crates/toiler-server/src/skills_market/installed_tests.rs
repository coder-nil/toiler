use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

const PACKAGE: &str = "owner/repo@demo";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("toiler-skill-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn root(&self) -> PathBuf {
        Scope::Project.root(&self.0, true).unwrap()
    }

    fn seed_legacy(&self, name: &str, files: &ArchiveFiles) -> InstallMetadata {
        let root = self.root();
        let package = parse_package(&format!("owner/repo@{name}")).unwrap();
        let bundle = select_skill(files, &package, None, false).unwrap();
        let target = root.join(name);
        let metadata = InstallMetadata {
            package: format!("owner/repo@{name}"),
            scope: "project".into(),
            source: package.source,
            source_type: "github".into(),
            skills_sh_url: format!("https://skills.sh/owner/repo/{name}"),
            skill_path: name.into(),
            version_hash: bundle.version_hash.clone(),
            can_check_for_updates: false,
            archive_path: None,
        };
        apply_bundle(&root, &target, &bundle, &metadata, false).unwrap();
        metadata
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn archive(files: &[(&str, &str)]) -> ArchiveFiles {
    let mut writer = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(content.len() as u64);
        writer
            .append_data(&mut header, format!("repo-HEAD/{path}"), content.as_bytes())
            .unwrap();
    }
    unpack_archive(&writer.into_inner().unwrap().finish().unwrap()).unwrap()
}

#[tokio::test]
async fn check_compares_each_skill_and_shares_one_archive_per_repository() {
    let fixture = Fixture::new();
    let old = archive(&[
        ("skills/demo/SKILL.md", "演示"),
        ("skills/other/SKILL.md", "不变"),
    ]);
    let installed = fixture.seed_legacy("demo", &old);
    fixture.seed_legacy("other", &old);
    let calls = &AtomicUsize::new(0);
    let response = check_with(
        None,
        Some(Scope::Project),
        &fixture.0,
        true,
        |package| async move {
            assert_eq!(package.source, "owner/repo");
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(archive(&[
                ("skills/demo/SKILL.md", "更新"),
                ("skills/other/SKILL.md", "不变"),
            ]))
        },
    )
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(response["updates"][0]["package"], PACKAGE);
    assert_eq!(response["updates"][0]["state"], "update-available");
    assert_eq!(
        response["updates"][0]["currentVersion"],
        installed.version_hash
    );
    assert_ne!(
        response["updates"][0]["latestVersion"],
        installed.version_hash
    );
    assert_eq!(response["updates"][1]["state"], "up-to-date");
    assert_eq!(
        response["updates"][1]["currentVersion"],
        response["updates"][1]["latestVersion"]
    );

    let error = check_with(
        Some(PACKAGE),
        Some(Scope::Project),
        &fixture.0,
        true,
        |_| async { Err(MarketError::upstream("模拟上游不可用")) },
    )
    .await
    .unwrap();
    assert_eq!(error["updates"][0]["state"], "error");
    assert_eq!(error["updates"][0]["message"], "模拟上游不可用");
    assert!(error["updates"][1].is_null());
}

#[tokio::test]
async fn update_preserves_old_install_on_error_and_replaces_files_on_success() {
    let fixture = Fixture::new();
    let old = archive(&[
        ("skills/demo/SKILL.md", "旧版"),
        ("skills/demo/obsolete.txt", "旧文件"),
    ]);
    let installed = fixture.seed_legacy("demo", &old);
    let root = fixture.root();
    let target = root.join("demo");
    let failed = update_with(PACKAGE, Scope::Project, &fixture.0, |_| async {
        Err(MarketError::timeout("GitHub"))
    })
    .await
    .unwrap_err();
    assert_eq!(failed.status, StatusCode::GATEWAY_TIMEOUT);
    let failed = update_with(PACKAGE, Scope::Project, &fixture.0, |_| async {
        // 上游归档中同一路径同时是文件与目录，落盘失败时旧目录不可动。
        Ok(archive(&[
            ("skills/demo/SKILL.md", "新版"),
            ("skills/demo/conflict", "文件"),
            ("skills/demo/conflict/child", "子文件"),
        ]))
    })
    .await
    .unwrap_err();
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "旧版");
    assert_eq!(
        Installed::read(&root, "demo", Scope::Project)
            .unwrap()
            .metadata
            .version_hash,
        installed.version_hash
    );

    let response = update_with(PACKAGE, Scope::Project, &fixture.0, |_| async {
        Ok(archive(&[
            ("skills/demo/SKILL.md", "新版"),
            ("skills/demo/sub/new.txt", "新文件"),
        ]))
    })
    .await
    .unwrap();
    assert_eq!(response["success"], true);
    assert_ne!(
        response["skill"]["install"]["versionHash"],
        installed.version_hash
    );
    assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "新版");
    assert!(target.join("sub/new.txt").is_file());
    assert!(!target.join("obsolete.txt").exists());

    let failed = update_with(PACKAGE, Scope::Project, &fixture.0, |_| async {
        Ok(archive(&[("skills/other/SKILL.md", "不是原来的技能")]))
    })
    .await
    .unwrap_err();
    assert_eq!(failed.status, StatusCode::BAD_REQUEST);
    assert_eq!(fs::read_to_string(target.join("SKILL.md")).unwrap(), "新版");
}

#[cfg(unix)]
#[tokio::test]
async fn remove_never_follows_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let outside = Fixture::new();
    let old = archive(&[("skills/demo/SKILL.md", "技能")]);
    outside.seed_legacy("demo", &old);
    let outside_root = outside.root();
    let root = fixture.root();
    fs::remove_dir(&root).unwrap();
    symlink(&outside_root, &root).unwrap();
    assert_eq!(
        remove(PACKAGE, Scope::Project, &fixture.0)
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
    fs::remove_file(&root).unwrap();
    fs::create_dir(&root).unwrap();
    symlink(outside_root.join("demo"), root.join("demo")).unwrap();
    assert_eq!(
        remove(PACKAGE, Scope::Project, &fixture.0)
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
    fs::remove_file(root.join("demo")).unwrap();
    fixture.seed_legacy("demo", &old);
    symlink(outside_root.join("demo"), root.join("demo/linked-resource")).unwrap();
    assert_eq!(
        remove(PACKAGE, Scope::Project, &fixture.0).await.unwrap()["success"],
        true
    );
    assert!(!root.join("demo").exists());
    assert!(outside_root.join("demo/SKILL.md").is_file());
}
