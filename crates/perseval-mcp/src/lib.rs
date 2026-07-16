#![forbid(unsafe_code)]

//! Stdio-only MCP boundary for Perseval's committed workspace service.

use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use perseval_service::{LiveTraceService, McpConfig, PersevalConfigV1, ServiceRuntime};
use rmcp::{ServiceExt, transport::stdio};
use tokio::sync::Notify;

mod cursor;
mod descriptors;
mod input;
pub mod ipc;
mod projection;
mod server;

use cursor::CursorCodec;

#[derive(Clone)]
pub struct PersevalMcp {
    pub(crate) runtime: ServiceRuntime,
    pub(crate) service: Arc<LiveTraceService>,
    pub(crate) workspace_id: String,
    pub(crate) policy: McpConfig,
    pub(crate) cursor: CursorCodec,
    pub(crate) initialized: Arc<AtomicBool>,
    pub(crate) initialized_notify: Arc<Notify>,
    pub(crate) initialize_seen: Arc<AtomicBool>,
}

impl std::fmt::Debug for PersevalMcp {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PersevalMcp")
            .field("workspace_id", &self.workspace_id)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl PersevalMcp {
    pub fn from_environment() -> Result<Self, McpStartError> {
        Self::start(PersevalConfigV1::load()?)
    }

    pub fn start(config: PersevalConfigV1) -> Result<Self, McpStartError> {
        validate_policy(&config.mcp)?;
        let workspace_id = config.workspace_id.clone();
        let policy = config.mcp.clone();
        let runtime = ServiceRuntime::start_mcp(config)?;
        Self::attach(runtime, workspace_id, policy)
    }

    pub fn attach(
        runtime: ServiceRuntime,
        workspace_id: String,
        policy: McpConfig,
    ) -> Result<Self, McpStartError> {
        validate_policy(&policy)?;
        let service = runtime
            .live()
            .cloned()
            .ok_or(McpStartError::WorkspaceUnavailable)?;
        Ok(Self {
            cursor: CursorCodec::new(&workspace_id, policy.cursor_ttl_seconds),
            runtime,
            service,
            workspace_id,
            policy,
            initialized: Arc::new(AtomicBool::new(false)),
            initialized_notify: Arc::new(Notify::new()),
            initialize_seen: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn runtime(&self) -> ServiceRuntime {
        self.runtime.clone()
    }

    pub async fn run_stdio(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let runtime = self.runtime.clone();
        let server = self.serve(stdio()).await?;
        let result = server.waiting().await;
        runtime.shutdown();
        result?;
        Ok(())
    }
}

fn validate_policy(policy: &McpConfig) -> Result<(), McpStartError> {
    if policy.compute_enabled || policy.write_enabled || policy.payload_reveal_enabled {
        Err(McpStartError::UnsupportedPermissionClass)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum McpStartError {
    Config(perseval_service::ConfigError),
    Runtime(perseval_service::RuntimeStartError),
    UnsupportedPermissionClass,
    WorkspaceUnavailable,
}

impl Display for McpStartError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(error) => Display::fmt(error, formatter),
            Self::Runtime(error) => Display::fmt(error, formatter),
            Self::UnsupportedPermissionClass => formatter.write_str(
                "this build implements the MCP read milestone; compute, write, and reveal permissions must remain disabled",
            ),
            Self::WorkspaceUnavailable => {
                formatter.write_str("the committed Perseval workspace is unavailable")
            }
        }
    }
}

impl std::error::Error for McpStartError {}

impl From<perseval_service::ConfigError> for McpStartError {
    fn from(error: perseval_service::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<perseval_service::RuntimeStartError> for McpStartError {
    fn from(error: perseval_service::RuntimeStartError) -> Self {
        Self::Runtime(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_permission_classes_fail_closed_at_startup() {
        let mut config = PersevalConfigV1::default();
        config.mcp.compute_enabled = true;

        assert!(matches!(
            PersevalMcp::start(config),
            Err(McpStartError::UnsupportedPermissionClass)
        ));
    }
}
