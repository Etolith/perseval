use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use perseval_service::{AnalysisStatus, CandidateGenerationJobStatusV1, RunSummary};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ErrorCode, Implementation, InitializeRequestParams,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
    Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::PersevalMcp;
use crate::cursor::CursorError;
use crate::descriptors::read_tools;
use crate::input::{
    GetEvalBatchJobInput, GetEvidenceTraceInput, GetFailureGroupInput, GetVerificationReportInput,
    GroupSort, InspectFindingInput, ListFailureGroupsInput, ListRunsInput, ListSessionsInput,
    PageInput, SessionSort, input_fingerprint,
};
use crate::projection;

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

impl ServerHandler for PersevalMcp {
    fn get_info(&self) -> InitializeResult {
        let mut capabilities = ServerCapabilities::builder().enable_tools().build();
        if let Some(tools) = &mut capabilities.tools {
            tools.list_changed = Some(false);
        }
        InitializeResult::new(capabilities)
            .with_protocol_version(ProtocolVersion::V_2025_11_25)
            .with_server_info(
                Implementation::new("perseval", env!("CARGO_PKG_VERSION")).with_title("Perseval"),
            )
            .with_instructions(
                "Use explicit project scopes. Trace payload bodies are omitted from safe read tools.",
            )
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, McpError>> + Send + '_ {
        if self.initialize_seen.swap(true, Ordering::AcqRel) {
            return std::future::ready(Err(McpError::invalid_request(
                "initialize may be called only once",
                None,
            )));
        }
        let protocol_version = request.protocol_version.clone();
        if protocol_version != ProtocolVersion::V_2025_06_18
            && protocol_version != ProtocolVersion::V_2025_11_25
        {
            return std::future::ready(Err(McpError::invalid_request(
                "Perseval MCP supports protocol versions 2025-06-18 and 2025-11-25",
                None,
            )));
        }
        context.peer.set_peer_info(request);
        let mut info = self.get_info();
        info.protocol_version = protocol_version;
        std::future::ready(Ok(info))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        if !self.await_initialized().await {
            return Err(McpError::invalid_request(
                "notifications/initialized is required before tools/list",
                None,
            ));
        }
        Ok(ListToolsResult::with_all_items(read_tools(
            self.policy.read_enabled,
        )))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        read_tools(self.policy.read_enabled)
            .into_iter()
            .find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if !self.await_initialized().await {
            return Err(McpError::invalid_request(
                "notifications/initialized is required before tools/call",
                None,
            ));
        }
        if !self.policy.read_enabled {
            return Err(method_not_found(&request.name));
        }
        match request.name.as_ref() {
            "list_projects" => self.list_projects(decode(request.arguments)?),
            "list_sessions" => self.list_sessions(decode(request.arguments)?),
            "list_runs" => self.list_runs(decode(request.arguments)?),
            "list_failure_groups" => self.list_failure_groups(decode(request.arguments)?),
            "get_failure_group" => self.get_failure_group(decode(request.arguments)?),
            "inspect_finding" => self.inspect_finding(decode(request.arguments)?),
            "get_evidence_trace" => self.get_evidence_trace(decode(request.arguments)?),
            "get_eval_batch_job" => self.get_eval_batch_job(decode(request.arguments)?),
            "get_verification_report" => self.get_verification_report(decode(request.arguments)?),
            _ => Err(method_not_found(&request.name)),
        }
    }

    fn on_initialized(
        &self,
        _context: NotificationContext<RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        self.initialized.store(true, Ordering::Release);
        self.initialized_notify.notify_waiters();
        std::future::ready(())
    }
}

impl PersevalMcp {
    async fn await_initialized(&self) -> bool {
        let notified = self.initialized_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.initialized.load(Ordering::Acquire) {
            return true;
        }
        let _ = tokio::time::timeout(Duration::from_millis(250), notified).await;
        self.initialized.load(Ordering::Acquire)
    }

    fn list_projects(&self, mut input: PageInput) -> Result<CallToolResult, McpError> {
        let tool = "list_projects";
        let limit = self.limit(input.limit, 200)?;
        let cursor_value = input.cursor.take();
        let filter_hash = input_fingerprint(&input);
        let commit_sequence = self.commit_sequence(tool)?;
        let offset = match self.offset(
            tool,
            None,
            &filter_hash,
            cursor_value.as_deref(),
            commit_sequence,
        ) {
            Ok(offset) => offset,
            Err(error) => return Ok(self.cursor_error(tool, error)),
        };
        let mut projects = self
            .service
            .list_projects()
            .map_err(|error| internal_error(tool, error))?;
        projects.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.project_id.cmp(&right.project_id))
        });
        let total = projects.len() as u64;
        let rows = projects
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .map(|project| projection::project(&project))
            .collect::<Vec<_>>();
        let next_cursor = self.next_cursor(
            tool,
            None,
            &filter_hash,
            offset,
            limit,
            total,
            commit_sequence,
        )?;
        self.success(
            tool,
            commit_sequence,
            None,
            json!({"projects": rows}),
            Vec::new(),
            next_cursor,
        )
    }

    fn list_sessions(&self, mut input: ListSessionsInput) -> Result<CallToolResult, McpError> {
        let tool = "list_sessions";
        let scope = input.scope.query_scope(true).map_err(invalid_params)?;
        let limit = self.limit(input.limit, 200)?;
        let cursor_value = input.cursor.take();
        let filter_hash = input_fingerprint(&input);
        let commit_sequence = self.commit_sequence(tool)?;
        let offset = match self.offset(
            tool,
            Some(&scope.scope_id),
            &filter_hash,
            cursor_value.as_deref(),
            commit_sequence,
        ) {
            Ok(offset) => offset,
            Err(error) => return Ok(self.cursor_error(tool, error)),
        };
        let filters = perseval_service::RunFiltersV1 {
            scope: scope.clone(),
            ..Default::default()
        };
        let runs = self
            .service
            .list_runs_filtered(&filters, offset, limit.saturating_add(1).min(200))
            .map_err(|error| internal_error(tool, error))?;
        let raw_count = runs.len();
        let sessions = session_summaries(runs, input.sort.unwrap_or_default(), limit as usize);
        let next_cursor = if raw_count > limit as usize {
            Some(
                self.cursor
                    .encode(
                        tool,
                        Some(&scope.scope_id),
                        &filter_hash,
                        offset + u64::from(limit),
                        commit_sequence,
                    )
                    .map_err(|error| internal_error(tool, error))?,
            )
        } else {
            None
        };
        self.success(
            tool,
            commit_sequence,
            Some(&scope.scope_id),
            json!({"sessions": sessions}),
            vec![warning(
                "session_projection_bounded",
                "Sessions are grouped from the committed bounded run page; missing session identities remain isolated per run.",
            )],
            next_cursor,
        )
    }

    fn list_runs(&self, mut input: ListRunsInput) -> Result<CallToolResult, McpError> {
        let tool = "list_runs";
        let filters = input.filters().map_err(invalid_params)?;
        let scope_id = filters.scope.scope_id.clone();
        let limit = self.limit(input.limit, 200)?;
        let cursor_value = input.cursor.take();
        let filter_hash = input_fingerprint(&input);
        let commit_sequence = self.commit_sequence(tool)?;
        let offset = match self.offset(
            tool,
            Some(&scope_id),
            &filter_hash,
            cursor_value.as_deref(),
            commit_sequence,
        ) {
            Ok(offset) => offset,
            Err(error) => return Ok(self.cursor_error(tool, error)),
        };
        let total = self
            .service
            .run_count_filtered(&filters)
            .map_err(|error| internal_error(tool, error))?;
        let mut rows = self
            .service
            .list_runs_filtered_ordered(
                &filters,
                input.sort.unwrap_or_default().into(),
                offset,
                limit,
            )
            .map_err(|error| internal_error(tool, error))?;
        if let Some(status) = input.analysis_status.first() {
            rows.retain(|run| status.matches(run.analysis_status));
        }
        if let Some(search) = input
            .search
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            let search = search.to_lowercase();
            rows.retain(|run| {
                run.title.to_lowercase().contains(&search)
                    || run.logical_trace_id.to_lowercase().contains(&search)
                    || run
                        .service_name
                        .as_deref()
                        .is_some_and(|value| value.to_lowercase().contains(&search))
            });
        }
        let data = rows.iter().map(projection::run).collect::<Vec<_>>();
        let mut warnings = Vec::new();
        if rows.iter().any(|run| {
            matches!(
                run.analysis_status,
                AnalysisStatus::Pending | AnalysisStatus::Analyzing | AnalysisStatus::Reanalyzing
            )
        }) {
            warnings.push(warning(
                "analysis_pending",
                "At least one finalized run still has analysis in progress; empty findings are not implied.",
            ));
        }
        let next_cursor = self.next_cursor(
            tool,
            Some(&scope_id),
            &filter_hash,
            offset,
            limit,
            total,
            commit_sequence,
        )?;
        self.success(
            tool,
            commit_sequence,
            Some(&scope_id),
            json!({"runs": data}),
            warnings,
            next_cursor,
        )
    }

    fn list_failure_groups(
        &self,
        mut input: ListFailureGroupsInput,
    ) -> Result<CallToolResult, McpError> {
        let tool = "list_failure_groups";
        let filters = input.filters().map_err(invalid_params)?;
        let scope_id = filters.scope.scope_id.clone();
        let limit = self.limit(input.limit, 200)?;
        let cursor_value = input.cursor.take();
        let filter_hash = input_fingerprint(&input);
        let commit_sequence = self.commit_sequence(tool)?;
        let offset = match self.offset(
            tool,
            Some(&scope_id),
            &filter_hash,
            cursor_value.as_deref(),
            commit_sequence,
        ) {
            Ok(offset) => offset,
            Err(error) => return Ok(self.cursor_error(tool, error)),
        };
        let mut page = self
            .service
            .list_failure_group_page(&filters, offset, limit)
            .map_err(|error| internal_error(tool, error))?;
        sort_groups(&mut page.rows, input.sort.unwrap_or_default());
        let groups = page
            .rows
            .iter()
            .map(projection::failure_group)
            .collect::<Vec<_>>();
        let next_cursor = self.next_cursor(
            tool,
            Some(&scope_id),
            &filter_hash,
            offset,
            limit,
            page.total,
            commit_sequence,
        )?;
        self.success(
            tool,
            commit_sequence,
            Some(&scope_id),
            json!({"groups": groups}),
            Vec::new(),
            next_cursor,
        )
    }

    fn get_failure_group(&self, input: GetFailureGroupInput) -> Result<CallToolResult, McpError> {
        let tool = "get_failure_group";
        let scope = input.scope.query_scope(true).map_err(invalid_params)?;
        let commit_sequence = self.commit_sequence(tool)?;
        let Some(detail) = self
            .service
            .get_failure_group_in_scope(&scope, &input.group_id)
            .map_err(|error| internal_error(tool, error))?
        else {
            return Ok(self.domain_error(
                tool,
                "not_found",
                "Failure group was not found in the supplied scope.",
                false,
            ));
        };
        let occurrences = self
            .service
            .list_failure_occurrences_in_scope(&scope, &input.group_id, 0, 20)
            .map_err(|error| internal_error(tool, error))?
            .iter()
            .map(projection::failure_occurrence)
            .collect::<Vec<_>>();
        self.success(
            tool,
            commit_sequence,
            Some(&scope.scope_id),
            projection::failure_group_detail(&detail, occurrences),
            Vec::new(),
            None,
        )
    }

    fn inspect_finding(&self, input: InspectFindingInput) -> Result<CallToolResult, McpError> {
        let tool = "inspect_finding";
        let scope = input.scope.query_scope(true).map_err(invalid_params)?;
        let commit_sequence = self.commit_sequence(tool)?;
        let Some(evidence) = self
            .service
            .get_finding_evidence_in_scope(&scope, &input.group_id, &input.finding_id)
            .map_err(|error| internal_error(tool, error))?
        else {
            return Ok(self.domain_error(
                tool,
                "not_found",
                "Finding was not found in the supplied scope.",
                false,
            ));
        };
        self.success(
            tool,
            commit_sequence,
            Some(&scope.scope_id),
            projection::finding(&evidence),
            Vec::new(),
            None,
        )
    }

    fn get_evidence_trace(
        &self,
        mut input: GetEvidenceTraceInput,
    ) -> Result<CallToolResult, McpError> {
        let tool = "get_evidence_trace";
        let scope = input.scope.query_scope(false).map_err(invalid_params)?;
        if input
            .maximum_spans
            .is_some_and(|requested| requested > self.policy.maximum_evidence_spans)
        {
            return Err(invalid_params(format!(
                "maximum_spans must not exceed {}",
                self.policy.maximum_evidence_spans
            )));
        }
        let maximum = input
            .maximum_spans
            .unwrap_or(self.policy.maximum_evidence_spans);
        if maximum == 0
            || input.before.unwrap_or_default() > maximum
            || input.after.unwrap_or_default() > maximum
        {
            return Err(invalid_params(
                "evidence context counts must fit within maximum_spans",
            ));
        }
        let cursor_value = input.cursor.take();
        let filter_hash = input_fingerprint(&input);
        let commit_sequence = self.commit_sequence(tool)?;
        let offset = match self.offset(
            tool,
            Some(&scope.scope_id),
            &filter_hash,
            cursor_value.as_deref(),
            commit_sequence,
        ) {
            Ok(offset) => offset,
            Err(error) => return Ok(self.cursor_error(tool, error)),
        };
        let Some(evidence) = self
            .service
            .get_finding_evidence_in_scope(&scope, &input.group_id, &input.finding_id)
            .map_err(|error| internal_error(tool, error))?
        else {
            return Ok(self.domain_error(
                tool,
                "not_found",
                "Evidence was not found in the supplied scope.",
                false,
            ));
        };
        let total = evidence.spans.len() as u64;
        let spans = evidence
            .spans
            .iter()
            .skip(offset as usize)
            .take(maximum as usize)
            .cloned()
            .collect::<Vec<_>>();
        let next_cursor = self.next_cursor(
            tool,
            Some(&scope.scope_id),
            &filter_hash,
            offset,
            maximum,
            total,
            commit_sequence,
        )?;
        let mut data = projection::evidence_trace(&evidence, &spans);
        if let Some(object) = data.as_object_mut() {
            object.insert("total_safe_spans".into(), json!(total.to_string()));
            object.insert(
                "omitted_span_count".into(),
                json!(
                    total
                        .saturating_sub(offset + spans.len() as u64)
                        .to_string()
                ),
            );
        }
        self.success(
            tool,
            commit_sequence,
            Some(&scope.scope_id),
            data,
            Vec::new(),
            next_cursor,
        )
    }

    fn get_eval_batch_job(&self, input: GetEvalBatchJobInput) -> Result<CallToolResult, McpError> {
        let tool = "get_eval_batch_job";
        let scope = input.scope.query_scope(false).map_err(invalid_params)?;
        let project_id = scope
            .criteria
            .project_id
            .as_deref()
            .expect("concrete scope was validated");
        let commit_sequence = self.commit_sequence(tool)?;
        let Some(job) = self
            .service
            .get_candidate_generation_job(&input.job_id)
            .map_err(|error| internal_error(tool, error))?
        else {
            return Ok(self.domain_error(
                tool,
                "not_found",
                "Eval batch job was not found.",
                false,
            ));
        };
        if job.project_id != project_id {
            return Ok(self.domain_error(
                tool,
                "not_found",
                "Eval batch job was not found in the supplied project.",
                false,
            ));
        }
        let terminal = matches!(
            job.status,
            CandidateGenerationJobStatusV1::Succeeded
                | CandidateGenerationJobStatusV1::PartialSuccess
                | CandidateGenerationJobStatusV1::Failed
                | CandidateGenerationJobStatusV1::Cancelled
        );
        let data = projection::eval_batch_job(
            &job,
            if terminal {
                0
            } else {
                self.policy.job_poll_interval_ms
            },
        );
        self.success(
            tool,
            commit_sequence,
            Some(&scope.scope_id),
            data,
            Vec::new(),
            None,
        )
    }

    fn get_verification_report(
        &self,
        input: GetVerificationReportInput,
    ) -> Result<CallToolResult, McpError> {
        let tool = "get_verification_report";
        let _scope = input.scope.query_scope(false).map_err(invalid_params)?;
        if input.job_id.is_some() == input.report_id.is_some() {
            return Err(invalid_params("provide exactly one of job_id or report_id"));
        }
        Ok(self.domain_error(
            tool,
            "not_found",
            "No remediation verification artifact matched this concrete project scope.",
            false,
        ))
    }

    fn commit_sequence(&self, tool: &str) -> Result<u64, McpError> {
        self.service
            .commit_sequence()
            .map_err(|error| internal_error(tool, error))
    }

    fn limit(&self, requested: Option<u32>, tool_maximum: u32) -> Result<u32, McpError> {
        let maximum = tool_maximum.min(self.policy.maximum_page_size);
        let limit = requested.unwrap_or(self.policy.default_page_size);
        if limit == 0 || limit > maximum {
            return Err(invalid_params(format!(
                "limit must be between 1 and {maximum}"
            )));
        }
        Ok(limit)
    }

    fn offset(
        &self,
        tool: &str,
        scope_id: Option<&str>,
        filter_hash: &str,
        cursor: Option<&str>,
        commit_sequence: u64,
    ) -> Result<u64, CursorError> {
        let Some(cursor) = cursor else {
            return Ok(0);
        };
        match self
            .cursor
            .decode(cursor, tool, scope_id, filter_hash, commit_sequence)
        {
            Ok(position) => Ok(position.offset),
            Err(error) => Err(error),
        }
    }

    fn cursor_error(&self, tool: &str, error: CursorError) -> CallToolResult {
        match error {
            CursorError::Invalid => self.domain_error(
                tool,
                "cursor_invalid",
                "The cursor is malformed, tampered with, or belongs to different inputs.",
                false,
            ),
            CursorError::Expired => self.domain_error(
                tool,
                "cursor_expired",
                "The committed cursor snapshot is no longer available; restart at page one.",
                true,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn next_cursor(
        &self,
        tool: &str,
        scope_id: Option<&str>,
        filter_hash: &str,
        offset: u64,
        limit: u32,
        total: u64,
        commit_sequence: u64,
    ) -> Result<Option<String>, McpError> {
        let next_offset = offset.saturating_add(u64::from(limit));
        if next_offset >= total {
            return Ok(None);
        }
        self.cursor
            .encode(tool, scope_id, filter_hash, next_offset, commit_sequence)
            .map(Some)
            .map_err(|error| internal_error(tool, error))
    }

    fn success(
        &self,
        tool: &str,
        commit_sequence: u64,
        scope_id: Option<&str>,
        data: Value,
        warnings: Vec<Value>,
        next_cursor: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        let mut envelope = Map::new();
        envelope.insert(
            "schema_version".into(),
            json!(format!("perseval.mcp.{tool}.output.v1")),
        );
        envelope.insert("ok".into(), json!(true));
        envelope.insert("request_id".into(), json!(request_id()));
        envelope.insert("workspace_id".into(), json!(self.workspace_id));
        envelope.insert("commit_sequence".into(), json!(commit_sequence.to_string()));
        if let Some(scope_id) = scope_id {
            envelope.insert("scope_id".into(), json!(scope_id));
        }
        envelope.insert("data".into(), data);
        envelope.insert("warnings".into(), Value::Array(warnings));
        if let Some(next_cursor) = next_cursor {
            envelope.insert("next_cursor".into(), json!(next_cursor));
        }
        let value = Value::Object(envelope);
        if serde_json::to_vec(&value)
            .map_err(|error| internal_error(tool, error))?
            .len()
            > self.policy.maximum_response_bytes
        {
            return Ok(self.domain_error(
                tool,
                "limit_exceeded",
                "The bounded result exceeds the configured MCP response limit; request a smaller page.",
                false,
            ));
        }
        Ok(CallToolResult::structured(value))
    }

    fn domain_error(
        &self,
        tool: &str,
        code: &str,
        message: &str,
        retryable: bool,
    ) -> CallToolResult {
        let value = json!({
            "schema_version": format!("perseval.mcp.{tool}.output.v1"),
            "ok": false,
            "request_id": request_id(),
            "error": {
                "code": code,
                "message": message,
                "retryable": retryable,
            }
        });
        CallToolResult::structured_error(value)
    }
}

fn decode<T: DeserializeOwned>(arguments: Option<Map<String, Value>>) -> Result<T, McpError> {
    serde_json::from_value(Value::Object(arguments.unwrap_or_default())).map_err(|error| {
        invalid_params(format!(
            "arguments do not match the advertised schema: {error}"
        ))
    })
}

fn method_not_found(name: &str) -> McpError {
    McpError::new(
        ErrorCode::METHOD_NOT_FOUND,
        format!("Unknown or disabled tool: {name}"),
        None,
    )
}

fn invalid_params(message: impl Into<String>) -> McpError {
    McpError::invalid_params(message.into(), None)
}

fn internal_error(tool: &str, error: impl std::fmt::Display) -> McpError {
    eprintln!("perseval-mcp {tool}: {error}");
    McpError::internal_error(
        format!("Perseval could not complete {tool}; see local diagnostics"),
        None,
    )
}

fn warning(code: &str, message: &str) -> Value {
    json!({"code": code, "message": message})
}

fn request_id() -> String {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("request:{time:x}-{sequence:x}")
}

fn sort_groups(groups: &mut [perseval_service::FailureGroupSummary], sort: GroupSort) {
    match sort {
        GroupSort::Priority => groups.sort_by(|left, right| {
            right
                .severity
                .cmp(&left.severity)
                .then_with(|| right.occurrence_count.cmp(&left.occurrence_count))
                .then_with(|| left.group_id.cmp(&right.group_id))
        }),
        GroupSort::MostFrequent => groups.sort_by(|left, right| {
            right
                .occurrence_count
                .cmp(&left.occurrence_count)
                .then_with(|| left.group_id.cmp(&right.group_id))
        }),
        GroupSort::Newest => groups.sort_by(|left, right| {
            right
                .last_seen_at
                .cmp(&left.last_seen_at)
                .then_with(|| left.group_id.cmp(&right.group_id))
        }),
        GroupSort::LargestIncrease => groups.sort_by(|left, right| {
            trend_increase(right)
                .cmp(&trend_increase(left))
                .then_with(|| left.group_id.cmp(&right.group_id))
        }),
    }
}

fn trend_increase(group: &perseval_service::FailureGroupSummary) -> i128 {
    match (
        group.occurrence_trend.first(),
        group.occurrence_trend.last(),
    ) {
        (Some(first), Some(last)) => i128::from(*last) - i128::from(*first),
        _ => 0,
    }
}

fn session_summaries(runs: Vec<RunSummary>, sort: SessionSort, limit: usize) -> Vec<Value> {
    #[derive(Default)]
    struct Session {
        project_id: String,
        session_id: String,
        run_count: u64,
        first_time: u64,
        last_time: u64,
        build_ids: BTreeSet<String>,
    }
    let mut sessions = BTreeMap::<(String, String), Session>::new();
    for run in runs {
        let session_id = run
            .session_id
            .clone()
            .unwrap_or_else(|| format!("missing:{}", run.logical_trace_id));
        let session = sessions
            .entry((run.project_id.clone(), session_id.clone()))
            .or_insert_with(|| Session {
                project_id: run.project_id.clone(),
                session_id,
                first_time: run.start_time_unix_nano,
                last_time: run.end_time_unix_nano,
                ..Session::default()
            });
        session.run_count += 1;
        session.first_time = session.first_time.min(run.start_time_unix_nano);
        session.last_time = session.last_time.max(run.end_time_unix_nano);
        if let Some(build_id) = run.build_id {
            session.build_ids.insert(build_id);
        }
    }
    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    match sort {
        SessionSort::Newest => sessions.sort_by(|left, right| {
            right
                .last_time
                .cmp(&left.last_time)
                .then_with(|| left.session_id.cmp(&right.session_id))
        }),
        SessionSort::Oldest => sessions.sort_by(|left, right| {
            left.first_time
                .cmp(&right.first_time)
                .then_with(|| left.session_id.cmp(&right.session_id))
        }),
        SessionSort::MostRuns => sessions.sort_by(|left, right| {
            right
                .run_count
                .cmp(&left.run_count)
                .then_with(|| left.session_id.cmp(&right.session_id))
        }),
    }
    sessions
        .into_iter()
        .take(limit)
        .map(|session| {
            json!({
                "project_id": session.project_id,
                "session_id": session.session_id,
                "run_count": session.run_count.to_string(),
                "first_time_unix_nano": session.first_time.to_string(),
                "last_time_unix_nano": session.last_time.to_string(),
                "build_ids": session.build_ids,
            })
        })
        .collect()
}
