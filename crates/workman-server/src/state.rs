//! 服务状态：会话存储、活 agent、模型目录、可信性策略、配置存储。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

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
    /// 模型目录。可替换：`qoder-cn` 的内置模型要等 `bind` 之后（知道端口）才能
    /// 注入，登录/退出也要重建一次（见 [`AppState::attach_qoder_cn`]）。
    pub catalog: RwLock<Arc<ModelCatalog>>,
    pub trust: TrustPolicy,
    /// 允许访问的根目录（git / 文件 / worktree 用）
    pub roots: AllowedRoots,
    /// `auth.json`
    pub auth: AuthStore,
    /// `models.json`
    pub models_file: ModelsFile,
    /// `settings.json`
    pub settings: SettingsFile,
    /// `work.sqlite` 里的项目表（见 `paths::project_db`）
    pub projects: ProjectStore,
    /// `trust.json`
    pub project_trust: TrustStore,
    /// 新会话默认 cwd
    pub default_cwd: PathBuf,
    /// 服务真正监听的地址（配对地址与 `desktopShell` 判定要用）。
    ///
    /// 单独用 `Arc<Mutex<..>>` 而不是普通字段：监听端口只有 `bind` 之后才知道，
    /// 而那时 `state` 往往已经被 clone 过（预热任务持有一份），拿不到 `&mut Inner`。
    pub binding: Arc<Mutex<Binding>>,
    /// `qoder-cn` 本机转发端点的路径令牌（进程内随机串，见 `qoder_cn::proxy`）。
    pub qoder_cn_token: String,
}

/// 服务绑定的地址；`port == 0` 表示还没绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub host: String,
    pub port: u16,
}

impl Default for Binding {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 0,
        }
    }
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
                catalog: RwLock::new(Arc::new(catalog)),
                trust: TrustPolicy::from_env(),
                roots: AllowedRoots::new(),
                auth: AuthStore::default(),
                models_file: ModelsFile::default(),
                settings: SettingsFile::default(),
                projects: ProjectStore::default(),
                project_trust: TrustStore::default(),
                default_cwd,
                binding: Arc::new(Mutex::new(Binding::default())),
                qoder_cn_token: uuid::Uuid::new_v4().simple().to_string(),
            }),
        }
    }

    /// 记录服务真正监听的地址。任意时刻调用都安全（共享槽），
    /// 已被 clone 的 state 也能看到新值。
    pub fn with_binding(self, host: impl Into<String>, port: u16) -> Self {
        let host = host.into();
        if let Ok(mut binding) = self.inner.binding.lock() {
            binding.host = host;
            binding.port = port;
        }
        self
    }

    pub fn server_port(&self) -> u16 {
        self.inner
            .binding
            .lock()
            .map(|binding| binding.port)
            .unwrap_or(0)
    }

    pub fn bind_host(&self) -> String {
        self.inner
            .binding
            .lock()
            .map(|binding| binding.host.clone())
            .unwrap_or_else(|_| "127.0.0.1".to_string())
    }

    /// 真实启动路径专用：把项目库指向 `<appDir>/work.sqlite` 并做一次旧库迁移。
    ///
    /// 与 [`Self::with_cwd`] 分开是故意的：`with_cwd` 是纯构造函数（测试大量
    /// 用它），绝不能在里面碰磁盘上的用户库；而迁移必须发生在真实启动时
    /// （CLI 与桌面壳都走 `boot::prepare_state`）。
    pub fn with_default_projects(mut self) -> Self {
        let store = ProjectStore::init_default();
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.projects = store;
        }
        self
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

    pub fn catalog(&self) -> Arc<ModelCatalog> {
        match self.inner.catalog.read() {
            Ok(guard) => Arc::clone(&guard),
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// 换一份模型目录（[`Self::attach_qoder_cn`] 与登录/退出后用）。
    pub fn replace_catalog(&self, catalog: ModelCatalog) {
        match self.inner.catalog.write() {
            Ok(mut guard) => *guard = Arc::new(catalog),
            Err(poisoned) => *poisoned.into_inner() = Arc::new(catalog),
        }
    }

    /// `qoder-cn` 本机转发端点的路径令牌。
    pub fn qoder_cn_token(&self) -> String {
        self.inner.qoder_cn_token.clone()
    }

    /// 把内置 `qoder-cn` 模型注入目录（幂等）。
    ///
    /// 必须在 `bind` 之后调用：模型的 `baseUrl` 要指向本进程的监听端口。
    /// 用户自己在 `models.json` 里写了 `qoder-cn` 时以用户的为准（不覆盖）。
    pub fn attach_qoder_cn(&self) -> bool {
        let port = self.server_port();
        if port == 0 {
            return false;
        }
        let current = self.catalog();
        if current
            .providers
            .iter()
            .any(|p| p.id() == crate::qoder_cn::PROVIDER_ID)
        {
            return false;
        }
        let base = crate::qoder_cn::local_base_url(port, &self.qoder_cn_token());
        // 只在 debug 级打出来：地址里含路径令牌（防局域网滥用），默认日志不落盘。
        tracing::debug!("qoder-cn 本机转发端点：{base}");
        let next = (*current).clone().with_qoder_cn(&base);
        self.replace_catalog(next);
        true
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
