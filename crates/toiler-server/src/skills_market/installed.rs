//! 管理 Toiler 自己安装的技能；扫描到的手工技能不是可写的包。

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::*;
use crate::paths;

const MAX_METADATA_BYTES: u64 = 16 * 1024;
// 只串行化本地提交，不在下载期间持锁。提交前重新校验快照，避免覆盖并发操作。
static MUTATION: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Project,
}

impl Scope {
    pub fn parse(value: &str) -> Result<Self, MarketError> {
        match value {
            "global" => Ok(Self::Global),
            "project" => Ok(Self::Project),
            _ => Err(MarketError::bad_request("scope 必须是 global 或 project")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }

    fn root(self, cwd: &Path, create: bool) -> Result<PathBuf, MarketError> {
        let home = paths::agents_home();
        let base = match self {
            Self::Global => {
                if create {
                    fs::create_dir_all(&home).map_err(local_error)?;
                }
                fs::canonicalize(&home).map_err(local_error)?
            }
            Self::Project => {
                let project =
                    fs::canonicalize(crate::skills::project_root(cwd)).map_err(local_error)?;
                if !fs::canonicalize(cwd)
                    .map_err(local_error)?
                    .starts_with(&project)
                {
                    return Err(failure(
                        StatusCode::FORBIDDEN,
                        "工作目录的真实路径不在项目根内",
                    ));
                }
                let base = child_dir(&project, ".agents", create)?;
                if fs::canonicalize(&home).is_ok_and(|global| global == base) {
                    return Err(failure(
                        StatusCode::FORBIDDEN,
                        "不能通过项目作用域管理全局技能",
                    ));
                }
                base
            }
        };
        child_dir(&base, "skills", create)
    }
}

fn failure(status: StatusCode, message: impl Into<String>) -> MarketError {
    MarketError {
        status,
        message: message.into(),
    }
}

fn local_error(error: io::Error) -> MarketError {
    match error.kind() {
        io::ErrorKind::NotFound => {
            failure(StatusCode::NOT_FOUND, "没有找到对应的 Toiler 技能安装记录")
        }
        io::ErrorKind::AlreadyExists => failure(StatusCode::CONFLICT, "技能目录已存在"),
        _ => {
            tracing::warn!(?error, "技能目录操作失败");
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "操作技能目录失败，请检查文件权限",
            )
        }
    }
}

fn child_dir(parent: &Path, name: &str, create: bool) -> Result<PathBuf, MarketError> {
    let path = parent.join(name);
    let info = match fs::symlink_metadata(&path) {
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&path).map_err(local_error)?;
            fs::symlink_metadata(&path)
        }
        result => result,
    }
    .map_err(local_error)?;
    // realpath 同时挡住 Windows junction；不能只检查最终的技能目录。
    if !info.is_dir() || fs::canonicalize(&path).map_err(local_error)? != path {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "不管理经由符号链接或目录联接的技能",
        ));
    }
    Ok(path)
}

fn regular_file(dir: &Path, name: &str) -> Result<PathBuf, MarketError> {
    let path = dir.join(name);
    if !fs::symlink_metadata(&path).map_err(local_error)?.is_file()
        || fs::canonicalize(&path).map_err(local_error)? != path
    {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "技能安装记录和 SKILL.md 必须是普通文件",
        ));
    }
    Ok(path)
}

#[derive(Debug, PartialEq, Eq)]
struct Installed {
    dir: PathBuf,
    metadata: InstallMetadata,
}

impl Installed {
    fn read(root: &Path, directory: &str, scope: Scope) -> Result<Self, MarketError> {
        validate_segment(directory).map_err(|_| MarketError::bad_request("技能名称无效"))?;
        let dir = child_dir(root, directory, false)?;
        let mut text = Vec::new();
        fs::File::open(regular_file(&dir, INSTALL_METADATA_FILE)?)
            .map_err(local_error)?
            .take(MAX_METADATA_BYTES + 1)
            .read_to_end(&mut text)
            .map_err(local_error)?;
        let invalid = || failure(StatusCode::CONFLICT, "技能安装记录无效，请手动管理该目录");
        if text.len() as u64 > MAX_METADATA_BYTES {
            return Err(invalid());
        }
        let mut metadata: InstallMetadata = serde_json::from_slice(&text).map_err(|_| invalid())?;
        let package = parse_package(&metadata.package).map_err(|_| invalid())?;
        if package.skill.as_deref() != Some(directory)
            || package.source != metadata.source
            || metadata.package != format!("{}@{directory}", package.source)
            || metadata.scope != scope.name()
            || metadata.skill_path != directory
            || metadata.source_type != "github"
            || metadata.version_hash.len() != 64
            || !metadata.version_hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid());
        }
        regular_file(&dir, "SKILL.md")?;
        // 兼容早期写了 false 的安装；无需迁移磁盘文件就能显示检查按钮。
        metadata.can_check_for_updates = true;
        metadata.skills_sh_url = format!("https://skills.sh/{}/{directory}", package.source);
        Ok(Self { dir, metadata })
    }

    fn requested(root: &Path, scope: Scope, package: &GithubPackage) -> Result<Self, MarketError> {
        let directory = package
            .skill
            .as_deref()
            .ok_or_else(|| MarketError::bad_request("请指定 owner/repo@skill"))?;
        let installed = Self::read(root, directory, scope)?;
        if installed.metadata.source != package.source {
            return Err(failure(
                StatusCode::NOT_FOUND,
                "没有找到对应的 Toiler 技能安装记录",
            ));
        }
        Ok(installed)
    }
}

pub fn metadata_for_skill_file(path: &Path, cwd: &Path, scope: Scope) -> Option<InstallMetadata> {
    let root = scope.root(cwd, false).ok()?;
    let dir = path.parent()?;
    if path.file_name()?.to_str()? != "SKILL.md" || fs::canonicalize(dir.parent()?).ok()? != root {
        return None;
    }
    let installed = Installed::read(&root, dir.file_name()?.to_str()?, scope).ok()?;
    // discovery 允许本地链接；安装管理不认链接别名。
    (fs::canonicalize(path).ok()? == installed.dir.join("SKILL.md")).then_some(installed.metadata)
}

pub async fn install(package: &str, scope: Scope, cwd: &Path) -> Result<Value, MarketError> {
    let package = parse_package(package)?;
    let files = fetch_archive(package.clone()).await?;
    let cwd = cwd.to_owned();
    blocking(move || {
        let bundle = select_skill(&files, &package, None, false)?;
        let directory = package.skill.as_deref().unwrap_or(&bundle.directory);
        validate_segment(directory).map_err(|_| MarketError::bad_request("技能目录名无效"))?;
        let _guard = lock_mutation()?;
        let root = scope.root(&cwd, true)?;
        let target = root.join(directory);
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                return Err(failure(
                    StatusCode::CONFLICT,
                    format!("技能目录已存在：{directory}"),
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(local_error(error)),
        }
        let metadata = InstallMetadata {
            package: format!("{}@{directory}", package.source),
            scope: scope.name().into(),
            source: package.source.clone(),
            source_type: "github".into(),
            skills_sh_url: format!("https://skills.sh/{}/{directory}", package.source),
            skill_path: directory.to_owned(),
            version_hash: bundle.version_hash.clone(),
            can_check_for_updates: true,
            archive_path: Some(bundle.archive_path.clone()),
        };
        apply_bundle(&root, &target, &bundle, &metadata, false)?;
        Ok(installed_response(&metadata))
    })
    .await
}

pub async fn check(
    package: Option<&str>,
    scope: Option<Scope>,
    cwd: &Path,
    trusted: bool,
) -> Result<Value, MarketError> {
    check_with(package, scope, cwd, trusted, fetch_archive).await
}

async fn check_with<F>(
    package: Option<&str>,
    scope: Option<Scope>,
    cwd: &Path,
    trusted: bool,
    fetch: impl Fn(GithubPackage) -> F,
) -> Result<Value, MarketError>
where
    F: std::future::Future<Output = Result<ArchiveFiles, MarketError>>,
{
    let package = package.map(parse_package).transpose()?;
    if package.as_ref().is_some_and(|pkg| pkg.skill.is_none()) {
        return Err(MarketError::bad_request("请指定 owner/repo@skill"));
    }
    let cwd = cwd.to_owned();
    let groups = blocking(move || {
        let _guard = lock_mutation()?;
        let mut groups = BTreeMap::<String, Vec<InstallMetadata>>::new();
        for candidate in [Scope::Global, Scope::Project] {
            if scope.is_some_and(|scope| scope != candidate)
                || (candidate == Scope::Project && !trusted)
            {
                continue;
            }
            let root = match candidate.root(&cwd, false) {
                Ok(root) => root,
                Err(error) if scope.is_none() && error.status == StatusCode::FORBIDDEN => continue,
                Err(error) if error.status == StatusCode::NOT_FOUND => continue,
                Err(error) => return Err(error),
            };
            let mut add = |entry: Installed| {
                groups
                    .entry(entry.metadata.source.clone())
                    .or_default()
                    .push(entry.metadata);
            };
            if let Some(package) = &package {
                match Installed::requested(&root, candidate, package) {
                    Ok(entry) => add(entry),
                    Err(error) if scope.is_none() && error.status == StatusCode::NOT_FOUND => (),
                    Err(error) => return Err(error),
                }
            } else {
                for entry in fs::read_dir(&root).map_err(local_error)? {
                    let entry = entry.map_err(local_error)?;
                    if let Some(name) = entry.file_name().to_str() {
                        if let Ok(entry) = Installed::read(&root, name, candidate) {
                            add(entry);
                        }
                    }
                }
            }
        }
        if package.is_some() && groups.is_empty() {
            return Err(failure(
                StatusCode::NOT_FOUND,
                "没有找到对应的 Toiler 技能安装记录",
            ));
        }
        Ok(groups)
    })
    .await?;

    // 同一个仓库可能有多项技能，或项目和全局各装一份；一次检查只取一次归档。
    let fetch = &fetch;
    let replies: Vec<_> =
        futures::stream::iter(groups.into_iter().map(|(source, entries)| async move {
            let package = parse_package(&source)?;
            match fetch(package).await {
                Ok(files) => {
                    blocking(move || {
                        Ok(entries
                            .iter()
                            .map(|entry| {
                                let result = parse_package(&entry.package).and_then(|package| {
                                    select_skill(
                                        &files,
                                        &package,
                                        entry.archive_path.as_deref(),
                                        true,
                                    )
                                });
                                check_result(
                                    entry,
                                    result.as_ref().map(|bundle| bundle.version_hash.as_str()),
                                )
                            })
                            .collect::<Vec<_>>())
                    })
                    .await
                }
                Err(error) => Ok(entries
                    .iter()
                    .map(|entry| check_result(entry, Err(&error)))
                    .collect()),
            }
        }))
        .buffer_unordered(3)
        .collect()
        .await;
    let mut updates: Vec<Value> = replies
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    updates.sort_unstable_by(|a, b| {
        (a["scope"].as_str(), a["package"].as_str())
            .cmp(&(b["scope"].as_str(), b["package"].as_str()))
    });
    Ok(json!({ "updates": updates }))
}

fn check_result(entry: &InstallMetadata, result: Result<&str, &MarketError>) -> Value {
    let mut value = json!({
        "package": entry.package,
        "scope": entry.scope,
        "currentVersion": entry.version_hash,
    });
    match result {
        Ok(latest) => {
            value["state"] = json!(if entry.version_hash == latest {
                "up-to-date"
            } else {
                "update-available"
            });
            value["latestVersion"] = json!(latest);
        }
        Err(error) => {
            value["state"] = json!("error");
            value["message"] = json!(error.message);
        }
    }
    value
}

pub async fn update(package: &str, scope: Scope, cwd: &Path) -> Result<Value, MarketError> {
    update_with(package, scope, cwd, fetch_archive).await
}

async fn update_with<F>(
    package: &str,
    scope: Scope,
    cwd: &Path,
    fetch: impl FnOnce(GithubPackage) -> F,
) -> Result<Value, MarketError>
where
    F: std::future::Future<Output = Result<ArchiveFiles, MarketError>>,
{
    let package = parse_package(package)?;
    let cwd = cwd.to_owned();
    let lookup_cwd = cwd.clone();
    let lookup_package = package.clone();
    let before = blocking(move || {
        let _guard = lock_mutation()?;
        Installed::requested(&scope.root(&lookup_cwd, false)?, scope, &lookup_package)
    })
    .await?;
    let files = fetch(package.clone()).await?;
    blocking(move || {
        let bundle = select_skill(
            &files,
            &package,
            before.metadata.archive_path.as_deref(),
            true,
        )?;
        let _guard = lock_mutation()?;
        let root = scope.root(&cwd, false)?;
        let current = Installed::requested(&root, scope, &package)?;
        if current != before {
            return Err(failure(
                StatusCode::CONFLICT,
                "技能安装记录在下载期间已变更，请重试",
            ));
        }
        let metadata = upgrade_bundle(&root, current, &bundle)?;
        Ok(installed_response(&metadata))
    })
    .await
}

fn upgrade_bundle(
    root: &Path,
    current: Installed,
    bundle: &SkillBundle<'_>,
) -> Result<InstallMetadata, MarketError> {
    let mut metadata = current.metadata;
    if metadata.version_hash != bundle.version_hash {
        metadata.version_hash.clone_from(&bundle.version_hash);
        metadata.archive_path = Some(bundle.archive_path.clone());
        apply_bundle(root, &current.dir, bundle, &metadata, true)?;
    }
    Ok(metadata)
}

pub async fn remove(package: &str, scope: Scope, cwd: &Path) -> Result<Value, MarketError> {
    let package = parse_package(package)?;
    let cwd = cwd.to_owned();
    blocking(move || {
        let _guard = lock_mutation()?;
        let root = scope.root(&cwd, false)?;
        let installed = Installed::requested(&root, scope, &package)?;
        let stage = staging_dir(&root)?;
        let result = fs::rename(&installed.dir, stage.join("removed")).map_err(local_error);
        cleanup(&stage);
        result?;
        Ok(json!({ "success": true }))
    })
    .await
}

fn installed_response(metadata: &InstallMetadata) -> Value {
    json!({
        "success": true,
        "package": metadata.package,
        "scope": metadata.scope,
        "skill": { "install": metadata },
    })
}

fn staging_dir(root: &Path) -> Result<PathBuf, MarketError> {
    // 放在 skills 根外，不让发现器在写到一半时加载新目录或再次加载备份。
    let stage = root.with_file_name(format!(".toiler-skill-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage).map_err(local_error)?;
    Ok(stage)
}

fn apply_bundle(
    root: &Path,
    target: &Path,
    bundle: &SkillBundle<'_>,
    metadata: &InstallMetadata,
    replace: bool,
) -> Result<(), MarketError> {
    let stage = staging_dir(root)?;
    let pending = stage.join("pending");
    let previous = stage.join("previous");
    let mut keep_backup = false;
    let result = (|| {
        fs::create_dir(&pending).map_err(local_error)?;
        write_bundle(&pending, &bundle.files, metadata)
            .map_err(|_| failure(StatusCode::INTERNAL_SERVER_ERROR, "写入技能文件失败"))?;
        if replace {
            fs::rename(target, &previous).map_err(local_error)?;
        }
        if let Err(error) = fs::rename(&pending, target) {
            if replace && fs::rename(&previous, target).is_err() {
                keep_backup = true;
                tracing::error!(path = %previous.display(), "技能升级回滚失败，已保留旧目录");
                return Err(failure(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("升级失败且无法还原，旧技能保留在 {}", previous.display()),
                ));
            }
            return Err(local_error(error));
        }
        Ok(())
    })();
    if !keep_backup {
        cleanup(&stage);
    }
    result
}

fn cleanup(stage: &Path) {
    // remove_dir_all 不追踪目录内的符号链接，旧目录里的链接目标不会被删除。
    if let Err(error) = fs::remove_dir_all(stage) {
        tracing::warn!(?error, path = %stage.display(), "无法清理技能临时目录");
    }
}

fn lock_mutation() -> Result<std::sync::MutexGuard<'static, ()>, MarketError> {
    MUTATION
        .lock()
        .map_err(|_| failure(StatusCode::INTERNAL_SERVER_ERROR, "无法锁定技能安装目录"))
}

async fn blocking<T: Send + 'static>(
    run: impl FnOnce() -> Result<T, MarketError> + Send + 'static,
) -> Result<T, MarketError> {
    tokio::task::spawn_blocking(run)
        .await
        .map_err(|_| failure(StatusCode::INTERNAL_SERVER_ERROR, "技能管理任务失败"))?
}

#[cfg(test)]
#[path = "installed_tests.rs"]
mod tests;
