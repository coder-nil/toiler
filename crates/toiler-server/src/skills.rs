//! 技能发现：目录约定、加载、去重 —— 面板与系统提示词共用的唯一事实来源。
//!
//! 两个消费者必须看到同一份结果：
//!
//! | 消费者 | 用途 |
//! |---|---|
//! | [`crate::routes::skills`] | `GET /api/skills` → 设置页的技能面板 |
//! | [`crate::system_prompt`] | `<skills>` 段 → 真正发给模型的技能清单 |
//!
//! 分叉过一次的代价是很具体的：面板上列着一堆「已启用」的技能，而模型的系统
//! 提示词里一个都没有（`<skills>` 段根本没被组装过），于是模型永远不知道有这些
//! 技能可用，用户却以为它们生效了。所以扫描规则只在这里写一遍。
//!
//! # 目录约定
//!
//! 只扫两处，一用户一项目：
//!
//! | 目录 | `source` / `scope` | 前端分组 |
//! |---|---|---|
//! | `<项目根>/.agents/skills` | `project` | Project |
//! | `~/.agents/skills` | `user` | Global |
//!
//! pi 用 `DefaultResourceLoader` 扫 `settings.json` 的 `skillPaths`、package 自带的
//! 技能，以及 `.agents/skills`（还会再扫 `~/.pi/agent/skills`）。toiler 没有
//! package 系统，`settings.json` 里也没有 `skillPaths` 键，**并且刻意不扫**
//! `~/.pi/agent/skills` 与 `~/.agents/toiler/skills`：技能目录只有一处约定，
//! 用户不必猜自己的技能为什么在一个地方生效、换个 agent 目录就失效。
//! 从 pi 迁过来的用户想继续用原有技能，把 `~/.pi/agent/skills` 里的目录
//! 搬（或链接）到 `~/.agents/skills` 即可——首次启动的配置播种只搬
//! `auth/models/settings/trust/git-credentials`，**不搬技能目录**。
//!
//! # 项目技能的两个前提
//!
//! 1. **只认项目根这一层**：约定位置是 `$project/.agents/skills`。项目根取
//!    「从 cwd 向上最近的 git 仓库根」，不在 git 里就是 cwd 自己。所以子目录里
//!    开会话（`$project/frontend`）也能看到项目根的技能，而仓库之外的父目录
//!    不会被误认成项目。
//! 2. **项目已信任**：未信任时 pi 根本不加载项目技能（`projectTrusted ?
//!    collectAncestorAgentsSkillDirs(...) : []`），这里保持一致——否则一个
//!    刚 clone 下来的仓库可以靠 `.agents/skills` 里的提示词影响 agent。
//!
//! 这两个条件也解释了为什么「不同项目会有不同的技能」，以及为什么刚建的
//! 项目目录里技能不会马上出现（需要先在设置里信任它）。
//!
//! # 去重规则
//!
//! 一句话：**项目与用户之间合并（同名都留），同一个文件只收录一次**。
//!
//! - 与 pi 的差异：pi 的 loader 用 `name` 做 key，项目里放一份同名技能会把全局
//!   那份顶掉。toiler 两份都保留 —— 项目技能本来就是「给这个项目换一套行为」，
//!   面板上应该能同时看到、分别查看。
//! - 同一个文件被扫到两次（`~/.agents/skills` 恰好就是项目根那一份时）才按
//!   `filePath` 去重。
//!
//! # 两处容易误导的「静默」行为
//!
//! - `SKILL.md` **缺 `description`** 时 `rpi` 直接丢弃、**不产生诊断**（与 pi 一致）。
//!   技能写好了却不出现，先查 frontmatter。
//! - 诊断只覆盖读取/解析失败；目录不存在不算错误（见 [`dirs`] 直接跳过）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rpi_harness::skills::{load_skills, SkillDiagnostic};
use rpi_harness::types::Skill;
use rpi_tools::{ExecutionEnv, OsExecutionEnv};

use crate::paths;

/// 技能来源。`source` 与 `scope` 在 pi 的 `sourceInfo` 里同值，这里就用一个枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Project,
    User,
}

impl Origin {
    /// `sourceInfo.source` 的值（前端按它分 Project / Global 组）。
    pub fn source(self) -> &'static str {
        match self {
            Origin::Project => "project",
            Origin::User => "user",
        }
    }

    /// `sourceInfo.scope` 的值。当前与 [`Self::source`] 相同。
    pub fn scope(self) -> &'static str {
        self.source()
    }
}

/// 一个要扫的技能目录，连同它代表的作用域。
#[derive(Debug, Clone)]
pub struct SkillDir {
    pub path: PathBuf,
    pub origin: Origin,
}

/// 加载成功的一个技能 + 它来自哪里。
#[derive(Debug, Clone)]
pub struct LoadedSkill {
    pub skill: Skill,
    pub origin: Origin,
}

/// 加载过程中的一条警告 + 它属于哪个作用域。
#[derive(Debug, Clone)]
pub struct SkillWarning {
    pub diagnostic: SkillDiagnostic,
    pub origin: Origin,
}

/// 一次扫描的全部结果。
#[derive(Debug, Clone, Default)]
pub struct Skills {
    pub skills: Vec<LoadedSkill>,
    pub warnings: Vec<SkillWarning>,
}

impl Skills {
    /// 按名字找技能。项目版排在前面，所以同名时拿到的是项目那份。
    pub fn get(&self, name: &str) -> Option<&LoadedSkill> {
        self.skills.iter().find(|loaded| loaded.skill.name == name)
    }
}

/// 要扫的技能目录，**按优先级排序**（先出现的胜出），且只返回真实存在的目录。
///
/// 只有两个来源：项目（只认项目根这一层）与用户（`~/.agents/skills`）。
/// 不存在的目录直接跳过而不是交给 `load_skills`：后者会为它产出 `list_failed`
/// 诊断，而「没装过项目技能」是常态，不该在界面上报 warning。
pub fn dirs(cwd: &Path, trusted: bool) -> Vec<SkillDir> {
    let mut out = Vec::new();
    let user_agents = paths::agents_home().join("skills");

    if trusted {
        // 只认**项目根**这一层：`$project/.agents/skills`。
        // 与 pi 的差异：pi 的 `collectAncestorAgentsSkillDirs` 在找不到 git 根时
        // 会一路扫到文件系统根，那样 `~/Development/.agents/skills` 这类「祖先共享
        // 目录」也会被当成项目技能。toiler 只认项目根，行为更好预测。
        let candidate = project_root(cwd).join(".agents").join("skills");
        // `~/.agents/skills` 在下面按 user 加，别重复标成 project。
        if candidate != user_agents {
            push_dir(&mut out, candidate, Origin::Project);
        }
    }

    push_dir(&mut out, user_agents, Origin::User);

    out
}

fn push_dir(out: &mut Vec<SkillDir>, path: PathBuf, origin: Origin) {
    if path.is_dir() && !out.iter().any(|existing| existing.path == path) {
        out.push(SkillDir { path, origin });
    }
}

/// 项目根：从 cwd 向上最近的 git 仓库根；不在任何 git 仓库里时就是 cwd 自己。
///
/// 「项目根」而不是「cwd」是刻意的：用户在子目录里开会话（`$project/frontend`）
/// 时，`$project/.agents/skills` 仍然要生效。
pub fn project_root(cwd: &Path) -> PathBuf {
    let mut dir = Some(cwd.to_path_buf());
    while let Some(current) = dir {
        if current.join(".git").exists() {
            return current;
        }
        dir = current.parent().map(Path::to_path_buf);
    }
    cwd.to_path_buf()
}

/// 按 [`dirs`] 的顺序扫描并合并成一份结果。
///
/// `trusted` 是项目是否已被信任（`crate::trust::status(...).trusted`）：未信任时
/// 项目技能整批不加载，与 pi 的 `projectTrusted` 开关同义。
pub async fn load(cwd: &Path, trusted: bool) -> Skills {
    load_dirs(cwd, dirs(cwd, trusted)).await
}

async fn load_dirs(cwd: &Path, dirs: Vec<SkillDir>) -> Skills {
    let env: Arc<dyn ExecutionEnv> = Arc::new(OsExecutionEnv::with_cwd(cwd.to_path_buf()));
    // 只按文件路径去重：同名技能是要保留两份的（项目版 + 全局版）。
    let mut seen_paths: HashSet<String> = HashSet::new();
    let mut out = Skills::default();

    for dir in dirs {
        let target = dir.path.to_string_lossy().to_string();
        let loaded = load_skills(&env, std::slice::from_ref(&target)).await;
        for diagnostic in loaded.diagnostics {
            out.warnings.push(SkillWarning {
                diagnostic,
                origin: dir.origin,
            });
        }
        for skill in loaded.skills {
            if !seen_paths.insert(skill.file_path.clone()) {
                continue;
            }
            out.skills.push(LoadedSkill {
                skill,
                origin: dir.origin,
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_root_prefers_nearest_git_root() {
        let root = std::env::temp_dir().join(format!("toiler-skill-root-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let nested = root.join("frontend/src");
        std::fs::create_dir_all(&nested).unwrap();

        // 不在 git 里：cwd 自己就是项目根。
        assert_eq!(project_root(&nested), nested);

        std::fs::create_dir_all(root.join(".git")).unwrap();
        assert_eq!(project_root(&nested), root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn origin_source_and_scope_match_pi_source_info() {
        // 前端按这两个字段把技能分到 Project / Global 组，改名字就要改前端。
        assert_eq!(Origin::Project.source(), "project");
        assert_eq!(Origin::Project.scope(), "project");
        assert_eq!(Origin::User.source(), "user");
        assert_eq!(Origin::User.scope(), "user");
    }

    #[test]
    fn only_two_sources_user_and_project() {
        // 用户级目录**只有一个**来源：`~/.agents/skills`。
        // 不扫 `~/.pi/agent/skills`、也不扫 `~/.agents/toiler/skills`：
        // 技能目录只有一处约定，用户不必猜自己的技能为何有时不生效。
        let cwd = std::env::temp_dir().join(format!("toiler-skill-dirs-{}", std::process::id()));
        let user_skills = paths::agents_home().join("skills");

        // 未信任时只剩用户级目录（若它存在），绝不会有项目那一份。
        let untrusted = dirs(&cwd, false);
        assert!(
            untrusted.iter().all(|dir| dir.origin == Origin::User),
            "未信任的项目不该出现项目技能目录"
        );
        assert!(
            untrusted.len() <= 1,
            "用户级只能有一个目录，实际：{:?}",
            untrusted.iter().map(|d| &d.path).collect::<Vec<_>>()
        );
        for dir in &untrusted {
            assert_eq!(dir.path, user_skills, "用户级目录必须是 ~/.agents/skills");
        }

        // 已信任时最多两个：项目根那一份 + 用户那一份。
        let trusted = dirs(&cwd, true);
        assert!(trusted.len() <= 2, "最多两个来源");
        let projects: Vec<&SkillDir> = trusted
            .iter()
            .filter(|dir| dir.origin == Origin::Project)
            .collect();
        assert!(projects.len() <= 1, "项目来源只能有一个");
        for dir in projects {
            assert!(
                dir.path.ends_with(".agents/skills"),
                "项目目录必须是 $project/.agents/skills：{:?}",
                dir.path
            );
        }
    }

    #[tokio::test]
    async fn project_and_user_skills_are_merged_even_with_same_name() {
        // 技能面板和系统提示词都用这份合并结果；不借用用户真实的技能目录。
        let root =
            std::env::temp_dir().join(format!("toiler-skill-merge-{}", uuid::Uuid::new_v4()));
        let dirs = [("project", Origin::Project), ("global", Origin::User)]
            .into_iter()
            .map(|(scope, origin)| {
                let path = root.join(scope).join(".agents/skills");
                let skill_dir = path.join("demo");
                std::fs::create_dir_all(&skill_dir).unwrap();
                std::fs::write(
                    skill_dir.join("SKILL.md"),
                    format!("---\nname: demo\ndescription: {scope} 版同名技能\n---\n"),
                )
                .unwrap();
                SkillDir { path, origin }
            })
            .collect();
        let found = load_dirs(&root, dirs).await;
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.skills.len(), 2);
        assert!(found
            .skills
            .iter()
            .all(|loaded| loaded.skill.name == "demo"));
        assert_eq!(found.skills[0].origin, Origin::Project);
        assert_eq!(found.skills[1].origin, Origin::User);
    }

    #[test]
    fn get_finds_by_name() {
        let mut skills = Skills::default();
        skills.skills.push(LoadedSkill {
            skill: Skill {
                name: "demo".to_string(),
                description: "d".to_string(),
                content: "c".to_string(),
                file_path: "/x/SKILL.md".to_string(),
                disable_model_invocation: None,
            },
            origin: Origin::User,
        });
        assert!(skills.get("demo").is_some());
        assert!(skills.get("missing").is_none());
    }
}
