//! 服务状态：会话存储、活 agent、模型目录、可信性策略。

use std::sync::Arc;

use crate::agent_runtime::LiveSessions;
use crate::catalog::ModelCatalog;
use crate::security::TrustPolicy;
use crate::sessions_store::SessionList;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub sessions: SessionList,
    pub live: LiveSessions,
    pub catalog: ModelCatalog,
    pub trust: TrustPolicy,
    /// 新会话默认 cwd
    pub default_cwd: std::path::PathBuf,
}

impl AppState {
    pub fn new(sessions_root: impl Into<std::path::PathBuf>, catalog: ModelCatalog) -> Self {
        Self::with_cwd(
            sessions_root,
            catalog,
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/")),
        )
    }

    pub fn with_cwd(
        sessions_root: impl Into<std::path::PathBuf>,
        catalog: ModelCatalog,
        default_cwd: impl Into<std::path::PathBuf>,
    ) -> Self {
        let default_cwd = default_cwd.into();
        Self {
            inner: Arc::new(Inner {
                sessions: SessionList::new(sessions_root),
                live: LiveSessions::new(),
                catalog,
                trust: TrustPolicy::from_env(),
                default_cwd,
            }),
        }
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

    pub fn default_cwd(&self) -> &std::path::Path {
        &self.inner.default_cwd
    }
}
