use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn seed_project(workspace: &std::path::Path) {
    let config = perseval_service::PersevalConfigV1 {
        workspace_id: "default".into(),
        workspace_dir: workspace.to_path_buf(),
        ..perseval_service::PersevalConfigV1::default()
    };
    let service = perseval_service::LiveTraceService::start(config).unwrap();
    service
        .create_project(perseval_service::CreateProjectV1 {
            project_id: "checkout-agent".into(),
            display_name: "Checkout agent".into(),
            artifact_namespace: "evals/checkout".into(),
        })
        .unwrap();
    service.shutdown();
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl McpProcess {
    fn start(workspace: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_perseval-mcp"))
            .env_remove("PERSEVAL_CONFIG")
            .env_remove("PERSEVAL_OTLP_ENABLED")
            .env_remove("PERSEVAL_MCP_COMPUTE_ENABLED")
            .env_remove("PERSEVAL_MCP_WRITE_ENABLED")
            .env_remove("PERSEVAL_MCP_PAYLOAD_REVEAL_ENABLED")
            .env("PERSEVAL_WORKSPACE_DIR", workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            stdin: child.stdin.take(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
        }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", serde_json::to_string(&message).unwrap()).unwrap();
        stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "MCP process closed stdout unexpectedly");
        assert_eq!(line.matches('\n').count(), 1);
        serde_json::from_str(&line).unwrap()
    }

    fn close_and_wait(mut self, expect_success: bool) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(7);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(
                    status.success(),
                    expect_success,
                    "MCP process exited with {status}"
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "MCP process did not drain after EOF"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

#[test]
fn stdio_initializes_lists_tools_and_returns_structured_safe_data() {
    let workspace = tempfile::tempdir().unwrap();
    seed_project(workspace.path());
    let mut mcp = McpProcess::start(workspace.path());
    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "perseval-test", "version": "1"}
        }
    }));
    let initialized = mcp.receive();
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        initialized["result"]["capabilities"],
        json!({"tools": {"listChanged": false}})
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "perseval");

    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": "premature",
        "method": "tools/list",
        "params": {}
    }));
    let premature = mcp.receive();
    assert_eq!(premature["error"]["code"], -32600);

    mcp.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tools = mcp.receive();
    let tools = tools["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 9);
    assert!(tools.iter().all(|tool| tool.get("outputSchema").is_some()));
    assert!(tools.iter().all(|tool| {
        tool["execution"]["taskSupport"] == "forbidden"
            && tool["annotations"]["readOnlyHint"] == true
    }));

    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": "list_projects", "arguments": {"limit": 10}}
    }));
    let response = mcp.receive();
    let result = &response["result"];
    assert_eq!(result["isError"], false);
    let structured = &result["structuredContent"];
    assert_eq!(structured["ok"], true);
    let projects = structured["data"]["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0]["project_id"], "checkout-agent");
    assert!(projects[0]["created_at_unix_ms"].is_string());
    assert_eq!(
        result["content"][0]["text"],
        serde_json::to_string(structured).unwrap()
    );
    assert!(!response.to_string().contains("payload"));

    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "list_projects",
            "arguments": {"limit": 10, "cursor": "tampered"}
        }
    }));
    let cursor_error = mcp.receive();
    assert_eq!(cursor_error["result"]["isError"], true);
    assert_eq!(
        cursor_error["result"]["structuredContent"]["error"]["code"],
        "cursor_invalid"
    );

    mcp.close_and_wait(true);
}

#[test]
fn codex_protocol_version_initializes_and_lists_tools() {
    let workspace = tempfile::tempdir().unwrap();
    let mut mcp = McpProcess::start(workspace.path());
    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "codex", "version": "0.144.5"}
        }
    }));
    let initialized = mcp.receive();
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");

    mcp.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tools = mcp.receive();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 9);

    mcp.close_and_wait(true);
}

#[test]
fn unsupported_protocol_version_is_rejected_without_negotiation_fallback() {
    let workspace = tempfile::tempdir().unwrap();
    let mut mcp = McpProcess::start(workspace.path());
    mcp.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2099-01-01",
            "capabilities": {},
            "clientInfo": {"name": "perseval-test", "version": "1"}
        }
    }));
    let response = mcp.receive();
    assert!(response.get("error").is_some());
    assert!(response.get("result").is_none());
    assert_eq!(
        response["error"]["message"],
        "Perseval MCP supports protocol versions 2025-06-18 and 2025-11-25"
    );
    mcp.close_and_wait(false);
}

#[test]
#[cfg(unix)]
fn two_stdio_clients_share_the_live_gui_workspace_owner() {
    let workspace = tempfile::tempdir().unwrap();
    let workspace_dir = workspace
        .path()
        .join("long-default-application-support-workspace-segment-".repeat(3));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let config = perseval_service::PersevalConfigV1 {
        workspace_id: "default".into(),
        workspace_dir: workspace_dir.clone(),
        ..perseval_service::PersevalConfigV1::default()
    };
    let runtime = perseval_service::ServiceRuntime::start_embedded(config.clone()).unwrap();
    runtime
        .live()
        .unwrap()
        .create_project(perseval_service::CreateProjectV1 {
            project_id: "refund-support-agent".into(),
            display_name: "Refund Support Agent".into(),
            artifact_namespace: "refund-support-agent".into(),
        })
        .unwrap();
    let owner = perseval_mcp::ipc::McpWorkspaceServer::start(&config, runtime.clone()).unwrap();

    let mut first = McpProcess::start(&workspace_dir);
    let mut second = McpProcess::start(&workspace_dir);
    for (id, client) in [(11, &mut first), (12, &mut second)] {
        client.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "concurrent-test", "version": "1"}
            }
        }));
        assert_eq!(client.receive()["result"]["protocolVersion"], "2025-11-25");
        client.send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));
        client.send(json!({
            "jsonrpc": "2.0",
            "id": id + 100,
            "method": "tools/call",
            "params": {"name": "list_projects", "arguments": {"limit": 10}}
        }));
        let response = client.receive();
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(
            response["result"]["structuredContent"]["data"]["projects"][0]["project_id"],
            "refund-support-agent"
        );
    }

    first.close_and_wait(true);
    second.close_and_wait(true);
    owner.shutdown();
    runtime.shutdown();
}
