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
use crate::settings::{self, ModelRef, SettingsFile};
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
    /// 活会话 Responses 请求的摘要转发注册表；请求结束即撤销令牌。
    pub summary_relay: Arc<crate::reasoning_summary::SummaryRelay>,
    /// `apisets` 目录正在联网刷新（防并发重复拉取，见 [`AppState::spawn_apisets_refresh`]）。
    pub apisets_refreshing: std::sync::atomic::AtomicBool,
    /// 手动刷新串行化；重复请求明确返回忙碌，不让旧请求覆盖较新的目录。
    pub models_refresh_gate: tokio::sync::Mutex<()>,
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
                summary_relay: Arc::new(crate::reasoning_summary::SummaryRelay::default()),
                apisets_refreshing: std::sync::atomic::AtomicBool::new(false),
                models_refresh_gate: tokio::sync::Mutex::new(()),
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
    /// 覆盖默认项目目录。必须在任何 `clone()` 之前调用（靠 `Arc::get_mut`）。
    ///
    /// 为什么不在 [`Self::with_cwd`] 里算：那里是纯构造函数，拿不到「上次选中的
    /// 项目」这种需要读盘的信息，而读盘又会与测试的「不许碰用户数据」硬要求冲突。
    pub fn with_default_cwd(mut self, default_cwd: impl Into<PathBuf>) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.default_cwd = default_cwd.into();
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

    /// 只为活会话包摘要适配，不改共享目录、模型地址或 oneshot 调用。
    pub fn dialogue_catalog(&self) -> ModelCatalog {
        let mut catalog = (*self.catalog()).clone();
        catalog.providers = self
            .inner
            .summary_relay
            .wrap_all(&catalog.providers, self.server_port());
        catalog
    }

    /// 换一份模型目录（[`Self::attach_qoder_cn`] 与登录/退出后用）。
    pub fn replace_catalog(&self, catalog: ModelCatalog) {
        match self.inner.catalog.write() {
            Ok(mut guard) => *guard = Arc::new(catalog),
            Err(poisoned) => *poisoned.into_inner() = Arc::new(catalog),
        }
    }

    /// 按 `models.json` 的当前内容重建目录，再对齐内置 provider。
    ///
    /// # 为什么 `PUT` / `DELETE /api/models-config` 必须调它
    ///
    /// 目录是**启动时一次性构建的快照**（[`crate::catalog::ModelCatalog::load`]），
    /// 而 `/api/models` 的 `modelList` 直接读这份快照。设置页保存 `models.json`
    /// 之后如果只写文件、不重建，用户刚加的 provider 与模型就**一个都不会出现在
    /// 对话框的模型切换器里** —— 界面上看是「加了但没生效」，只有重启进程才会出现。
    ///
    /// 删除同理：被删掉的 provider 会继续留在 `modelList` 里，选了就报
    /// `Model not found`（[`crate::agent_runtime::LiveAgent::set_model`] 从目录里
    /// 查不到模型）。
    ///
    /// 重建后同步把新目录推给活会话（见 [`Self::sync_live_catalogs`]），否则
    /// 已经在内存里的会话切换模型仍会报找不到。
    ///
    /// 返回重建是否成功；失败时保留旧目录（宁可显示旧清单，也不要清空成空列表）。
    pub async fn reload_catalog_from_models_file(&self) -> anyhow::Result<()> {
        let path = self.models_file().path().to_path_buf();
        let raw = self.models_file().read();
        // 沿用旧目录的默认项：重建前用户可能已经在界面上选过模型，
        // `apply_default_hint` 会把它保住（`models.json` 里顺序变了也不会跳）。
        let current = self.catalog();
        let default_entry = current
            .default_entry()
            .map(|entry| (entry.provider.clone(), entry.id.clone()));
        let mut next = crate::catalog::ModelCatalog::from_config(raw, Some(path))?;
        next.apply_default_hint(default_entry);
        self.replace_catalog(next);
        // 目录换了之后内置 provider（`qoder-cn` / `apisets` / 远端缓存）要重新注入：
        // 它们不在 `models.json` 里，`from_config` 建不出来。
        self.attach_builtin_providers();
        self.sync_live_catalogs().await;
        Ok(())
    }

    /// 把当前目录推给所有活会话。
    ///
    /// [`LiveAgent`] 各自持有一份 `ModelCatalog` 副本（它比一次请求活得久，不能
    /// 借用 `AppState`），所以设置页保存 `models.json` 后必须挨个对齐，否则：
    ///
    /// - 界面上的 `modelList` 已经有新模型（`/api/models` 读 `AppState` 的目录）；
    /// - 但 `set_model` 走的是活会话自己那份旧目录，报 `Model not found`。
    ///
    /// 用户看到的是「选择器里有、一点就报错」。反过来删模型时也要推：旧目录里
    /// 还留着已删的模型，切过去同样会失败。
    ///
    /// 用 [`Self::dialogue_catalog`] 而不是 [`Self::catalog`]：活会话的目录必须带
    /// 摘要转发包装，否则推完之后切模型的会话就丢掉了推理摘要（两者的差异见
    /// [`Self::dialogue_catalog`] 的注释）。
    ///
    /// 单个会话推失败不影响其它会话，也不让保存操作回滚：harness 已 `closed`
    /// 意味着它正在 `shutdown`，那份目录副本已经没人用了。
    pub async fn sync_live_catalogs(&self) {
        let catalog = self.dialogue_catalog();
        for agent in self.live().all() {
            if let Err(error) = agent.replace_catalog(catalog.clone()).await {
                tracing::warn!("同步活会话的模型目录失败（session={}）：{error}", agent.id);
            }
        }
    }

    /// `qoder-cn` 本机转发端点的路径令牌。
    pub fn qoder_cn_token(&self) -> String {
        self.inner.qoder_cn_token.clone()
    }

    /// 对齐内置 `qoder-cn` 模型目录（幂等，返回目录是否变更）。
    ///
    /// 必须在 `bind` 之后调用：模型的 `baseUrl` 要指向本进程的监听端口。
    /// 内置快照仅用作订阅登录后的目录兜底；退出后要摘掉旧模型，避免仍可选中。
    /// 用户自己在 `models.json` 里写了 `qoder-cn` 时以用户的为准（不覆盖）。
    pub fn attach_qoder_cn(&self) -> bool {
        let port = self.server_port();
        let provider_id = crate::qoder_cn::PROVIDER_ID;
        if port == 0 || self.models_define(provider_id) {
            return false;
        }
        let current = self.catalog();
        let has_oauth = self
            .auth()
            .read()
            .get(provider_id)
            .and_then(crate::qoder_cn::auth::identity_from_credential)
            .is_some();
        let models = if has_oauth {
            let base = crate::qoder_cn::local_base_url(port, &self.qoder_cn_token());
            let entries = crate::qoder_cn::catalog::entries(self.models_file().path(), self.auth());
            crate::qoder_cn::models_from_catalog(&base, &entries)
        } else {
            Vec::new()
        };
        let installed: Vec<_> = current
            .models
            .iter()
            .filter(|model| model.provider == provider_id)
            .collect();
        if installed.len() == models.len()
            && installed.iter().zip(&models).all(|(old, new)| {
                old.id == new.id
                    && old.name == new.name
                    && old.base_url == new.base_url
                    && old.reasoning == new.reasoning
                    && old.input == new.input
                    && old.context_window == new.context_window
                    && old.max_tokens == new.max_tokens
                    && old.thinking_level_map == new.thinking_level_map
            })
        {
            return false;
        }
        let default_entry = current
            .default_entry()
            .map(|entry| (entry.provider.clone(), entry.id.clone()));
        let mut next = if models.is_empty() {
            (*current).clone().without_provider(provider_id)
        } else {
            (*current).clone().with_qoder_cn_models(models)
        };
        next.apply_default_hint(default_entry);
        self.replace_catalog(next);
        true
    }

    /// 把内置 `apisets`（设置页里的 **Coding**）目录注入目录（幂等，且会替换旧清单）。
    ///
    /// 目录来源是远端缓存（`models-store.json`，见 [`crate::apisets`]）；没有缓存、
    /// 没有 API Key、或者用户在 `models.json` 里自己写了同名 provider 时，只把旧的
    /// apisets 摘掉（退回到「没有模型」而不是留着过期清单）。
    ///
    /// 不联网：网络刷新走 [`Self::spawn_apisets_refresh`]。
    pub fn attach_apisets(&self) -> bool {
        if self.models_define(crate::apisets::PROVIDER_ID) {
            // 用户自己写了 `apisets` 时以用户的为准（与 `qoder-cn` 同一条规则）。
            return false;
        }
        let current = self.catalog();
        let default_entry = current
            .default_entry()
            .map(|entry| (entry.provider.clone(), entry.id.clone()));
        let key = self.auth().api_key(crate::apisets::PROVIDER_ID);
        let defs = key
            .as_deref()
            .map(|_| crate::apisets::cached_defs(self.inner.models_file.path()))
            .unwrap_or_default();
        let mut next = (*current).clone();
        next = match key.as_deref() {
            Some(key) if !defs.is_empty() => next.with_apisets(key, &defs),
            _ => next.without_provider(crate::apisets::PROVIDER_ID),
        };
        let installed = !defs.is_empty() && key.is_some();
        if !installed
            && !current
                .providers
                .iter()
                .any(|provider| provider.id() == crate::apisets::PROVIDER_ID)
        {
            // 本来就没装、现在也没东西可装：不必换目录，省一次全量克隆。
            return false;
        }
        next.apply_default_hint(default_entry);
        self.replace_catalog(next);
        installed
    }

    /// 把普通内置 API Key provider 的缓存目录注入运行时目录。
    ///
    /// 用户配置与远端模型取并集；同 id 的模型始终优先采用用户的元数据。
    /// 缓存 URL/协议与当前配置不一致时不注入，避免代理切换后读到另一家的目录。
    pub fn attach_remote_api_key_providers(&self) {
        let root = self.models_file().read();
        let current = self.catalog();
        let default_entry = current
            .default_entry()
            .map(|entry| (entry.provider.clone(), entry.id.clone()));
        let mut next = (*current).clone();
        for builtin in crate::builtin_providers::all() {
            if !builtin.api_key
                || builtin.id == crate::apisets::PROVIDER_ID
                || builtin.id == crate::qoder_cn::PROVIDER_ID
            {
                continue;
            }
            let config = crate::models_json::provider_config(&root, builtin.id);
            let key = config
                .and_then(|config| {
                    crate::routes::models_config::resolve_api_key(self, builtin.id, config)
                })
                .or_else(|| self.auth().api_key(builtin.id));
            let Some(cached) = crate::remote_models::read(self.models_file().path(), builtin.id)
            else {
                continue;
            };
            let base_url = config
                .and_then(|value| value["baseUrl"].as_str())
                .unwrap_or(&cached.base_url);
            let api = config
                .and_then(|value| value["api"].as_str())
                .unwrap_or(&cached.api);
            let mut defs = config
                .and_then(|value| value["models"].as_array())
                .cloned()
                .unwrap_or_default();
            let mut seen: std::collections::BTreeSet<String> = defs
                .iter()
                .filter_map(|value| value["id"].as_str().map(str::to_string))
                .collect();
            if key.is_some() && base_url == cached.base_url && api == cached.api {
                defs.extend(cached.models.into_iter().filter(|value| {
                    value["id"]
                        .as_str()
                        .is_some_and(|id| seen.insert(id.to_string()))
                }));
            }
            next = next.with_remote_provider(
                builtin.id,
                api,
                base_url,
                key.as_deref().unwrap_or(""),
                &defs,
            );
        }
        next.apply_default_hint(default_entry);
        self.replace_catalog(next);
    }

    /// 把内置 provider 全部重新对齐一遍（`qoder-cn` + `apisets` + 远端 API Key 目录）。
    ///
    /// 登录 / 退出 / 存或删 API Key 之后都必须调一次：目录是快照，凭据变了就要重算。
    pub fn attach_builtin_providers(&self) {
        self.attach_qoder_cn();
        self.attach_apisets();
        self.attach_remote_api_key_providers();
    }

    /// 后台拉一次 apisets 目录（成功则落盘并重新注入目录）。
    ///
    /// 返回 false 表示没有发起（没有 key、用户自己定义了 `apisets`、不在 async 上下文、
    /// 或已经有一个刷新在跑）。调用方不需要等它：设置页那条「重试」按钮会再来问一次。
    pub fn spawn_apisets_refresh(&self) -> bool {
        if self.models_define(crate::apisets::PROVIDER_ID) {
            return false;
        }
        let Some(key) = self.auth().api_key(crate::apisets::PROVIDER_ID) else {
            return false;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        if self
            .inner
            .apisets_refreshing
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            // 已经有一份在拉，重复请求直接当作「已排上」—— 否则 `refreshing` 的轮询
            // 会变成并发打上游。
            return true;
        }
        let state = self.clone();
        handle.spawn(async move {
            let _refresh_guard = state.inner.models_refresh_gate.lock().await;
            let models_json = state.inner.models_file.path().to_path_buf();
            match crate::apisets::refresh(&key, &models_json).await {
                Ok(defs) => {
                    tracing::info!("apisets 模型目录已刷新：{} 个模型", defs.len());
                    state.attach_apisets();
                    state.sessions().invalidate();
                }
                Err(error) => {
                    // 不降级成 warning：面板上会显示空列表，用户得能从日志里看到原因。
                    tracing::warn!("apisets 模型目录刷新失败：{error:#}");
                }
            }
            state
                .inner
                .apisets_refreshing
                .store(false, std::sync::atomic::Ordering::SeqCst);
        });
        true
    }

    /// 启动预热：缓存过期（或根本没有）时后台拉一份，让对话的模型下拉里立刻能用。
    ///
    /// 返回是否真的排上了任务；同步调用方（CLI / 桌面启动）不需要 await。
    pub fn warm_apisets_catalog(&self) -> bool {
        if self.models_define(crate::apisets::PROVIDER_ID) {
            return false;
        }
        if self.auth().api_key(crate::apisets::PROVIDER_ID).is_none() {
            return false;
        }
        let fresh = crate::apisets::read_store(self.inner.models_file.path())
            .map(|store| store.is_fresh() && !store.usable_defs().is_empty())
            .unwrap_or(false);
        if fresh {
            return false;
        }
        self.spawn_apisets_refresh()
    }

    /// `models.json` 里是否自己定义了某个 provider（自己定义时内置目录不插手）。
    fn models_define(&self, provider_id: &str) -> bool {
        crate::models_json::providers(&self.inner.models_file.read())
            .iter()
            .any(|(name, _)| name == provider_id)
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

    /// 目录里的全部模型（`enabledModels` 过滤前的全集）。
    pub fn catalog_model_refs(&self) -> Vec<ModelRef> {
        self.catalog()
            .entries
            .iter()
            .map(|entry| ModelRef {
                provider: entry.provider.clone(),
                id: entry.id.clone(),
            })
            .collect()
    }

    /// `enabledModels` 白名单里的模型（= 模型选择器里看得见的那些）。
    pub fn visible_model_refs(&self) -> Vec<ModelRef> {
        let patterns = self.settings().enabled_models();
        settings::visible_models(patterns.as_deref(), &self.catalog_model_refs())
    }

    /// 默认模型：**模型选择器里看得见**的那一个。
    ///
    /// 顺序与 pi 的 `selectInitialModelScope`（`lib/model-scope.ts`）一致：
    ///
    /// | 顺序 | 来源 |
    /// |---|---|
    /// | 1 | `settings.json` 的 `defaultProvider`/`defaultModel` |
    /// | 2 | [`ModelCatalog::default_entry`]（启动时由 `TOILER_MODEL` / 上次用过的模型定下） |
    /// | 3 | 可见范围里第一个能解析的模型 |
    ///
    /// # 为什么每一步都要校验 `enabledModels` 可见范围
    ///
    /// `/api/models` 的 `modelList` 只发可见模型，而前端的 `loadModels()` 拿
    /// `defaultModel` 去 `modelList` 里 `find`，**找不到就退回 `modelList[0]`**
    /// （`useAgentSession.ts`）：
    ///
    /// ```ts
    /// const displayModel = match ?? nextModelList[0];
    /// ```
    ///
    /// 所以「设置里选的默认模型」被 `enabledModels` 挡在可见范围外时，输入框会
    /// 静默显示成列表里排第一的那个模型 —— 而真正建会话用的仍是设置里那个。
    /// 用户看到选择器写着 A、实际对话走 B，且切换前后界面都不给任何提示。
    ///
    /// 历史故障正是这一条：`/api/models` 的 `defaultModel` 走的是
    /// `catalog.default_entry()`（`models.json` 里第一个可用项 = `gpt-5.4`），
    /// 它不在 `enabledModels` 里，于是界面显示 `modelList[0]`（`GPT 5.6 Sol`），
    /// 实际会话是 `settings.json` 的 `apisets/deepseek-v4.1-flash`。
    pub fn default_model_ref(&self) -> Option<(String, String)> {
        let catalog = self.catalog();
        let visible = self.visible_model_refs();
        let usable = |provider: &str, id: &str| {
            visible.iter().any(|m| m.provider == provider && m.id == id)
                && catalog.model_for(provider, id).is_some()
        };

        let (provider, model) = self.settings().default_model();
        if let (Some(provider), Some(model)) = (provider, model) {
            if usable(&provider, &model) {
                return Some((provider, model));
            }
        }
        if let Some(entry) = catalog.default_entry() {
            if usable(&entry.provider, &entry.id) {
                return Some((entry.provider.clone(), entry.id.clone()));
            }
        }
        visible
            .into_iter()
            .find(|m| catalog.model_for(&m.provider, &m.id).is_some())
            .map(|m| (m.provider, m.id))
    }

    /// 已有会话的模型优先于全局偏好。选择器与运行时共用这条规则，避免恢复时分家。
    pub(crate) fn session_model_ref(
        &self,
        history: Option<&serde_json::Value>,
    ) -> Option<(String, String)> {
        let catalog = self.catalog();
        history
            .and_then(|history| {
                let provider = history.get("provider")?.as_str()?;
                let id = history.get("modelId")?.as_str()?;
                catalog.model_for(provider, id)?;
                Some((provider.to_string(), id.to_string()))
            })
            .or_else(|| self.default_model_ref())
            .or_else(|| {
                catalog
                    .default_model()
                    .map(|m| (m.provider.clone(), m.id.clone()))
            })
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
