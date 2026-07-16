use std::time::Instant;

use gpui::{App, AppContext, Focusable};
use perseval_service::{PersevalConfigV1, ServiceRuntime};

use super::runtime_mode::RuntimeMode;
use super::window::workbench_window;
use crate::components::init_text_input;
use crate::screens::failure_inbox::init_key_bindings as init_failure_inbox_key_bindings;
use crate::screens::trace_fixture::{Workbench as TraceFixtureWorkbench, WorkbenchSettings};
use crate::screens::workbench_shell::{WorkbenchShell, init_key_bindings};

const PROFILE_STARTUP_ENV: &str = "PERSEVAL_PROFILE_STARTUP";

pub struct PersevalApp {
    runtime: ServiceRuntime,
}

impl PersevalApp {
    pub fn new() -> Self {
        Self {
            runtime: ServiceRuntime::embedded_gui(),
        }
    }

    pub fn runtime(&self) -> ServiceRuntime {
        self.runtime.clone()
    }

    pub fn run(self) {
        match RuntimeMode::detect() {
            RuntimeMode::Embedded => self.run_live(),
            RuntimeMode::Fixture => self.run_fixture(),
        }
    }

    fn run_fixture(self) {
        let profile_started = std::env::var_os(PROFILE_STARTUP_ENV).map(|_| Instant::now());
        let catalog = self.runtime.trace_catalog();
        if let Some(started) = profile_started {
            eprintln!(
                "perseval_profile catalog_ready_ms={:.3}",
                started.elapsed().as_secs_f64() * 1_000.0
            );
        }
        let settings = WorkbenchSettings::from_environment();
        gpui_platform::application()
            .with_assets(crate::icons::PersevalAssets)
            .run(move |cx: &mut App| {
                init_text_input(cx);
                cx.open_window(workbench_window("Perseval — Trace Workbench"), |_, cx| {
                    cx.new(move |_| TraceFixtureWorkbench::new(catalog, settings, profile_started))
                })
                .expect("open Perseval window");
                cx.activate(true);
            });
    }

    fn run_live(self) {
        let config = PersevalConfigV1::load().expect("load Perseval configuration");
        let shell_config = config.clone();
        let runtime = ServiceRuntime::start_embedded(config).expect("start Perseval service");
        let mcp_workspace =
            perseval_mcp::ipc::McpWorkspaceServer::start(&shell_config, runtime.clone())
                .map_err(|error| eprintln!("perseval: MCP workspace service unavailable: {error}"))
                .ok();
        let service = runtime.live().expect("live service is available").clone();
        let (snapshot, subscription) = runtime
            .snapshot_and_subscribe()
            .expect("take initial trace snapshot");
        let runtime_for_window = runtime.clone();
        gpui_platform::application()
            .with_assets(crate::icons::PersevalAssets)
            .run(move |cx: &mut App| {
                init_key_bindings(cx);
                init_failure_inbox_key_bindings(cx);
                // Register the context-specific input bindings after global workbench
                // shortcuts so Escape clears an active field before it bubbles to
                // dismiss the surrounding surface.
                init_text_input(cx);
                let service = service.clone();
                cx.open_window(
                    workbench_window("Perseval — Live Trace Workbench"),
                    |window, cx| {
                        let shell = cx.new(move |cx| {
                            WorkbenchShell::new(&shell_config, service, snapshot, subscription, cx)
                        });
                        shell.focus_handle(cx).focus(window, cx);
                        shell
                    },
                )
                .expect("open Perseval live window");
                cx.activate(true);
            });
        if let Some(mcp_workspace) = mcp_workspace {
            mcp_workspace.shutdown();
        }
        runtime_for_window.shutdown();
    }
}

impl Default for PersevalApp {
    fn default() -> Self {
        Self::new()
    }
}
