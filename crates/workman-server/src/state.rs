//! 服务状态：会话存储、活 agent、模型目录、可信性策略、配置存储。

use std::path::PathBuf;
use std::sync::Arc;

use crate::agent_runtime::LiveSessions;
use crate::auth::AuthStore;
use crate::catalog::ModelCatalog;
use crate::models_json::ModelsFile;
use crate::paths::AllowedRoots;
use crate::projects::ProjectStore;
use crate::security::TrustPolicy;
use crate::sessions_store::SessionList;
use crate::settings::SettingsFile;
use crate::trust::TrustStore;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub sessions: SessionList,
    pub live: LiveSessions,
    pub catalog: ModelCatalog,
    pub trust: TrustPolicy,
    /// 允许访问的根目录（git / 文件 / worktree 用）
    pub roots: AllowedRoots,
    /// `auth.json`
    pub auth: AuthStore,
    /// `models.json`
    pub models_file: ModelsFile,
    /// `settings.json`
    pub settings: SettingsFile,
    /// `pi.sqlite` 里的项目表
    pub projects: ProjectStore,
    /// `trust.json`
    pub project_trust: TrustStore,
    /// 新会话默认 cwd
    pub default_cwd: PathBuf,
}

impl AppState {
    pub fn new(sessions_root: impl Into<PathBuf>, catalog: ModelCatalog) -> Self {
        Self::with_cwd(
            sessions_root,
            catalog,
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        )
    }

    pub fn with_cwd(
        sessions_root: impl Into<PathBuf>,
        catalog: ModelCatalog,
        default_cwd: impl Into<PathBuf>,
    ) -> Self {
        let default_cwd = default_cwd.into();
        Self {
            inner: Arc::new(Inner {
                sessions: SessionList::new(sessions_root),
                live: LiveSessions::new(),
                catalog,
                trust: TrustPolicy::from_env(),
                roots: AllowedRoots::new(),
                auth: AuthStore::default(),
                models_file: ModelsFile::default(),
                settings: SettingsFile::default(),
                projects: ProjectStore::default(),
                project_trust: TrustStore::default(),
                default_cwd,
            }),
        }
    }

    /// 覆盖各配置文件路径（测试用）。
    pub fn with_stores(
        mut self,
        auth: AuthStore,
        models: ModelsFile,
        settings: SettingsFile,
        projects: ProjectStore,
        project_trust: TrustStore,
    ) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("配置只能在 clone 之前设置");
        inner.auth = auth;
        inner.models_file = models;
        inner.settings = settings;
        inner.projects = projects;
        inner.project_trust = project_trust;
        self
    }

    pub fn sessions(&self) -> &SessionList {
        &self.inner.sessions
    }

    pub fn live(&self) -> &LiveSessions {
        &self.inner.live
    }

    pub fn catalog(&self) -> &ModelCatalog {
        &self.inner.catalog
    }

    pub fn trust(&self) -> &TrustPolicy {
        &self.inner.trust
    }

    pub fn roots(&self) -> &AllowedRoots {
        &self.inner.roots
    }

    pub fn auth(&self) -> &AuthStore {
        &self.inner.auth
    }

    pub fn models_file(&self) -> &ModelsFile {
        &self.inner.models_file
    }

    pub fn settings(&self) -> &SettingsFile {
        &self.inner.settings
    }

    pub fn projects(&self) -> &ProjectStore {
        &self.inner.projects
    }

    pub fn project_trust(&self) -> &TrustStore {
        &self.inner.project_trust
    }

    pub fn default_cwd(&self) -> &std::path::Path {
        &self.inner.default_cwd
    }

    /// 默认模型：`settings.json` 的 `defaultProvider`/`defaultModel` 优先，
    /// 其次 [`ModelCatalog::default_entry`]（两者都要能在目录里解析到）。
    pub fn default_model_ref(&self) -> Option<(String, String)> {
        let (provider, model) = self.settings().default_model();
        if let (Some(provider), Some(model)) = (provider, model) {
            if self.catalog().model_for(&provider, &model).is_some() {
                return Some((provider, model));
            }
        }
        self.catalog()
            .default_entry()
            .map(|entry| (entry.provider.clone(), entry.id.clone()))
    }

    /// 把「会话里出现的 cwd / projectRoot + 默认 cwd + 已存项目」灌进允许根。
    ///
    /// 对应 pi 的 `80547.o()`：每次判定前重建一次集合（pi 有 5 秒缓存，
    /// 这里的会话列表本身就有 30 秒缓存，够用）。
    pub fn sync_allowed_roots(&self) {
        let mut seed: Vec<PathBuf> = vec![self.default_cwd().to_path_buf()];
        for item in self.sessions().list(false) {
            for key in ["cwd", "projectRoot"] {
                if let Some(value) = item.get(key).and_then(serde_json::Value::as_str) {
                    if !value.is_empty() {
                        seed.push(PathBuf::from(value));
                    }
                }
            }
        }
        for project in self.projects().list().unwrap_or_default() {
            seed.push(PathBuf::from(&project.project_root));
            seed.push(PathBuf::from(&project.cwd));
        }
        if let Ok(Some((_, root))) = self.projects().selected() {
            seed.push(PathBuf::from(root));
        }
        self.roots().seed(seed);
    }
}
