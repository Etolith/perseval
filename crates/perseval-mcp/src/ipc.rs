//! Private workspace-owner transport behind the public stdio MCP process.

use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;

use perseval_service::{PersevalConfigV1, ServiceRuntime};
use rmcp::ServiceExt;
use tokio::sync::oneshot;

use crate::PersevalMcp;

const SOCKET_NAME: &str = ".perseval-mcp-v1.sock";

pub fn socket_path(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join(SOCKET_NAME)
}

pub async fn run_stdio_entrypoint() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = PersevalConfigV1::load()?;
    if proxy_to_workspace_owner(&config.workspace_dir).await? {
        return Ok(());
    }
    PersevalMcp::start(config)?.run_stdio().await
}

#[cfg(unix)]
async fn proxy_to_workspace_owner(
    workspace_dir: &Path,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncWriteExt, copy};
    use tokio::net::UnixStream;

    let stream = match UnixStream::connect(socket_path(workspace_dir)).await {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(Box::new(error)),
    };
    let (mut reader, mut writer) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let input = async {
        copy(&mut stdin, &mut writer).await?;
        writer.shutdown().await
    };
    let output = async {
        copy(&mut reader, &mut stdout).await?;
        stdout.shutdown().await
    };
    tokio::try_join!(input, output)?;
    Ok(true)
}

#[cfg(not(unix))]
async fn proxy_to_workspace_owner(
    _workspace_dir: &Path,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    Ok(false)
}

pub struct McpWorkspaceServer {
    socket_path: PathBuf,
    shutdown: Mutex<Option<oneshot::Sender<()>>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl McpWorkspaceServer {
    #[cfg(unix)]
    pub fn start(
        config: &PersevalConfigV1,
        runtime: ServiceRuntime,
    ) -> Result<Self, IpcStartError> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixStream as StdUnixStream;

        let path = socket_path(&config.workspace_dir);
        if path.exists() {
            if StdUnixStream::connect(&path).is_ok() {
                return Err(IpcStartError::AlreadyRunning(path));
            }
            std::fs::remove_file(&path)?;
        }
        let workspace_id = config.workspace_id.clone();
        let policy = config.mcp.clone();
        let thread_path = path.clone();
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
        let thread = thread::Builder::new()
            .name("perseval-mcp-workspace".into())
            .spawn(move || {
                let tokio = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name("perseval-mcp-ipc")
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error.to_string()));
                        return;
                    }
                };
                tokio.block_on(async move {
                    let listener = match tokio::net::UnixListener::bind(&thread_path) {
                        Ok(listener) => listener,
                        Err(error) => {
                            let _ = ready_sender.send(Err(error.to_string()));
                            return;
                        }
                    };
                    if let Err(error) = std::fs::set_permissions(
                        &thread_path,
                        std::fs::Permissions::from_mode(0o600),
                    ) {
                        let _ = ready_sender.send(Err(error.to_string()));
                        return;
                    }
                    let _ = ready_sender.send(Ok(()));
                    let mut shutdown_receiver = shutdown_receiver;
                    loop {
                        tokio::select! {
                            _ = &mut shutdown_receiver => break,
                            accepted = listener.accept() => {
                                let Ok((stream, _)) = accepted else { continue };
                                let runtime = runtime.clone();
                                let workspace_id = workspace_id.clone();
                                let policy = policy.clone();
                                tokio::spawn(async move {
                                    let Ok(handler) = PersevalMcp::attach(runtime, workspace_id, policy) else {
                                        return;
                                    };
                                    if let Ok(server) = handler.serve(stream).await {
                                        let _ = server.waiting().await;
                                    }
                                });
                            }
                        }
                    }
                });
            })
            .map_err(|error| IpcStartError::Runtime(error.to_string()))?;
        ready_receiver
            .recv()
            .map_err(|error| IpcStartError::Runtime(error.to_string()))?
            .map_err(IpcStartError::Runtime)?;
        Ok(Self {
            socket_path: path,
            shutdown: Mutex::new(Some(shutdown_sender)),
            thread: Mutex::new(Some(thread)),
        })
    }

    #[cfg(not(unix))]
    pub fn start(
        _config: &PersevalConfigV1,
        _runtime: ServiceRuntime,
    ) -> Result<Self, IpcStartError> {
        Err(IpcStartError::Unsupported)
    }

    pub fn shutdown(&self) {
        if let Some(sender) = self.shutdown.lock().expect("MCP shutdown lock").take() {
            let _ = sender.send(());
        }
        if let Some(thread) = self.thread.lock().expect("MCP thread lock").take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for McpWorkspaceServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Debug)]
pub enum IpcStartError {
    AlreadyRunning(PathBuf),
    Io(std::io::Error),
    Runtime(String),
    Unsupported,
}

impl Display for IpcStartError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning(path) => write!(
                formatter,
                "an MCP workspace owner is already listening at {}",
                path.display()
            ),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Runtime(error) => formatter.write_str(error),
            Self::Unsupported => formatter.write_str("private MCP workspace IPC is unavailable"),
        }
    }
}

impl std::error::Error for IpcStartError {}

impl From<std::io::Error> for IpcStartError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
