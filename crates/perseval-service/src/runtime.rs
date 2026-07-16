use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, Mutex};
use std::thread;

use perseval_ingest::otlp::{OtlpReceiverConfig, serve_otlp};
use tokio::sync::oneshot;

use crate::config::PersevalConfigV1;
use crate::live::{LiveServiceError, LiveTraceService, TraceSnapshot, TraceSubscription};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    EmbeddedGui,
    McpStdio,
    HeadlessDaemon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub filesystem_watchers: bool,
    pub otlp_listener: bool,
    pub stdio_mcp: bool,
}

struct RuntimeInner {
    mode: RuntimeMode,
    capabilities: RuntimeCapabilities,
    live: Option<Arc<LiveTraceService>>,
    otlp_shutdown: Mutex<Option<oneshot::Sender<()>>>,
    otlp_thread: Mutex<Option<thread::JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct ServiceRuntime {
    inner: Arc<RuntimeInner>,
}

impl std::fmt::Debug for ServiceRuntime {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceRuntime")
            .field("mode", &self.inner.mode)
            .field("capabilities", &self.inner.capabilities)
            .finish_non_exhaustive()
    }
}

impl ServiceRuntime {
    pub fn embedded_gui() -> Self {
        Self::marker(RuntimeMode::EmbeddedGui)
    }

    pub fn start_embedded(config: PersevalConfigV1) -> Result<Self, RuntimeStartError> {
        if config.otlp.enabled && std::env::var_os(crate::queries::TRACE_FILE_ENV).is_some() {
            return Err(RuntimeStartError::FixtureAndOtlpConflict);
        }
        let live = LiveTraceService::start(config.clone())?;
        let mut capabilities = RuntimeCapabilities {
            filesystem_watchers: true,
            otlp_listener: false,
            stdio_mcp: false,
        };
        let mut shutdown = None;
        let mut server_thread = None;
        if config.otlp.enabled {
            let receiver_config = OtlpReceiverConfig {
                enabled: true,
                bind_addr: config.otlp.bind_addr,
                source_id: config.otlp.source_id.clone(),
                max_wire_bytes: config.otlp.max_wire_bytes,
                max_decoded_bytes: config.otlp.max_decoded_bytes,
                max_spans_per_request: config.otlp.max_spans_per_request,
                max_attributes_per_span: config.otlp.max_attributes_per_span,
                retry_after_seconds: config.otlp.retry_after_seconds,
            };
            let (shutdown_sender, shutdown_receiver) = oneshot::channel();
            let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
            let sink = Arc::new(live.ingest_handle());
            let thread = thread::Builder::new()
                .name("perseval-otlp-http".into())
                .spawn(move || {
                    let runtime = match tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .thread_name("perseval-otlp-worker")
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            let _ = ready_sender.send(Err(error.to_string()));
                            return;
                        }
                    };
                    runtime.block_on(async move {
                        match tokio::net::TcpListener::bind(receiver_config.bind_addr).await {
                            Ok(listener) => {
                                let address =
                                    listener.local_addr().map_err(|error| error.to_string());
                                let _ = ready_sender.send(address);
                                let _ = serve_otlp(listener, receiver_config, sink, async move {
                                    let _ = shutdown_receiver.await;
                                })
                                .await;
                            }
                            Err(error) => {
                                let _ = ready_sender.send(Err(error.to_string()));
                            }
                        }
                    });
                })
                .map_err(|error| RuntimeStartError::Listener(error.to_string()))?;
            let address = ready_receiver
                .recv()
                .map_err(|error| RuntimeStartError::Listener(error.to_string()))?
                .map_err(RuntimeStartError::Listener)?;
            live.set_effective_address(Some(address.to_string()));
            capabilities.otlp_listener = true;
            shutdown = Some(shutdown_sender);
            server_thread = Some(thread);
        }
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                mode: RuntimeMode::EmbeddedGui,
                capabilities,
                live: Some(live),
                otlp_shutdown: Mutex::new(shutdown),
                otlp_thread: Mutex::new(server_thread),
            }),
        })
    }

    pub fn mcp_stdio() -> Self {
        Self::marker(RuntimeMode::McpStdio)
    }

    /// Opens one committed workspace for the stdio MCP process without
    /// enabling ingestion transports, filesystem watchers, or a listener.
    pub fn start_mcp(mut config: PersevalConfigV1) -> Result<Self, RuntimeStartError> {
        config.otlp.enabled = false;
        let live = LiveTraceService::start(config)?;
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                mode: RuntimeMode::McpStdio,
                capabilities: RuntimeCapabilities {
                    filesystem_watchers: false,
                    otlp_listener: false,
                    stdio_mcp: true,
                },
                live: Some(live),
                otlp_shutdown: Mutex::new(None),
                otlp_thread: Mutex::new(None),
            }),
        })
    }

    pub fn headless_daemon() -> Self {
        Self::marker(RuntimeMode::HeadlessDaemon)
    }

    fn marker(mode: RuntimeMode) -> Self {
        Self {
            inner: Arc::new(RuntimeInner {
                mode,
                capabilities: RuntimeCapabilities {
                    filesystem_watchers: mode != RuntimeMode::McpStdio,
                    otlp_listener: false,
                    stdio_mcp: mode == RuntimeMode::McpStdio,
                },
                live: None,
                otlp_shutdown: Mutex::new(None),
                otlp_thread: Mutex::new(None),
            }),
        }
    }

    pub fn with_otlp_listener(self) -> Result<Self, RuntimeConfigurationError> {
        if self.inner.mode == RuntimeMode::McpStdio {
            return Err(RuntimeConfigurationError::OtlpNotAllowedForMcp);
        }
        Ok(self)
    }

    pub fn mode(&self) -> RuntimeMode {
        self.inner.mode
    }

    pub fn capabilities(&self) -> RuntimeCapabilities {
        self.inner.capabilities
    }

    pub fn live(&self) -> Option<&Arc<LiveTraceService>> {
        self.inner.live.as_ref()
    }

    pub fn snapshot_and_subscribe(
        &self,
    ) -> Result<(TraceSnapshot, TraceSubscription), RuntimeStartError> {
        self.inner
            .live
            .as_ref()
            .ok_or(RuntimeStartError::NotStarted)?
            .snapshot_and_subscribe()
            .map_err(RuntimeStartError::Live)
    }

    pub fn trace_catalog(&self) -> traces_to_evals::Result<crate::queries::TraceCatalog> {
        crate::queries::TraceCatalog::from_environment()
    }

    pub fn shutdown(&self) {
        if let Some(sender) = self
            .inner
            .otlp_shutdown
            .lock()
            .expect("OTLP shutdown lock poisoned")
            .take()
        {
            let _ = sender.send(());
        }
        if let Some(thread) = self
            .inner
            .otlp_thread
            .lock()
            .expect("OTLP thread lock poisoned")
            .take()
        {
            let _ = thread.join();
        }
        if let Some(live) = &self.inner.live {
            live.shutdown();
        }
    }
}

#[derive(Debug)]
pub enum RuntimeStartError {
    Live(LiveServiceError),
    Listener(String),
    FixtureAndOtlpConflict,
    NotStarted,
}

impl From<LiveServiceError> for RuntimeStartError {
    fn from(error: LiveServiceError) -> Self {
        Self::Live(error)
    }
}

impl Display for RuntimeStartError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live(error) => Display::fmt(error, formatter),
            Self::Listener(error) => write!(formatter, "could not start OTLP listener: {error}"),
            Self::FixtureAndOtlpConflict => formatter.write_str(
                "PERSEVAL_TRACE_FILE fixture mode cannot be combined with live OTLP ingestion",
            ),
            Self::NotStarted => formatter.write_str("service runtime was not started"),
        }
    }
}

impl Error for RuntimeStartError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeConfigurationError {
    OtlpNotAllowedForMcp,
}

impl Display for RuntimeConfigurationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OtlpNotAllowedForMcp => {
                formatter.write_str("the stdio MCP runtime cannot open the OTLP listener")
            }
        }
    }
}

impl Error for RuntimeConfigurationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gui_uses_filesystem_ingestion_without_a_listener_by_default() {
        let runtime = ServiceRuntime::embedded_gui();
        assert!(runtime.capabilities().filesystem_watchers);
        assert!(!runtime.capabilities().otlp_listener);
        assert!(!runtime.capabilities().stdio_mcp);
    }

    #[test]
    fn mcp_is_stdio_only() {
        let runtime = ServiceRuntime::mcp_stdio();
        assert!(!runtime.capabilities().filesystem_watchers);
        assert!(!runtime.capabilities().otlp_listener);
        assert!(runtime.capabilities().stdio_mcp);
        assert_eq!(
            runtime.with_otlp_listener().unwrap_err(),
            RuntimeConfigurationError::OtlpNotAllowedForMcp
        );
    }

    #[test]
    fn started_mcp_runtime_forces_network_ingestion_off() {
        let workspace = tempfile::tempdir().unwrap();
        let mut config = PersevalConfigV1 {
            workspace_dir: workspace.path().to_path_buf(),
            ..PersevalConfigV1::default()
        };
        config.otlp.enabled = true;

        let runtime = ServiceRuntime::start_mcp(config).unwrap();
        assert!(!runtime.capabilities().filesystem_watchers);
        assert!(!runtime.capabilities().otlp_listener);
        assert!(runtime.capabilities().stdio_mcp);
        assert!(runtime.live().is_some());
        runtime.shutdown();
    }
}
