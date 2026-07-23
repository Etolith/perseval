use super::*;

impl WorkspaceStore {
    pub fn list_runs(&self, offset: u64, limit: u32) -> Result<Vec<RunSummary>, StoreError> {
        self.list_runs_filtered_ordered(&RunFiltersV1::default(), RunOrderV1::Newest, offset, limit)
    }

    pub fn list_runs_filtered(
        &self,
        filters: &RunFiltersV1,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<RunSummary>, StoreError> {
        self.list_runs_filtered_ordered(filters, RunOrderV1::Newest, offset, limit)
    }

    pub fn list_runs_filtered_ordered(
        &self,
        filters: &RunFiltersV1,
        order: RunOrderV1,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<RunSummary>, StoreError> {
        filters.scope.validate().map_err(StoreError::Invalid)?;
        let scope = &filters.scope.criteria;
        let control = self.control.lock().expect("control store lock poisoned");
        let ordering = match order {
            RunOrderV1::Newest => "start_time_unix_nano DESC, logical_trace_id",
            RunOrderV1::Oldest => "start_time_unix_nano ASC, logical_trace_id",
            RunOrderV1::MostSpans => "span_count DESC, start_time_unix_nano DESC, logical_trace_id",
            RunOrderV1::MostFindings => {
                "finding_count DESC, start_time_unix_nano DESC, logical_trace_id"
            }
        };
        let project_predicate = if scope.project_id.as_deref().unwrap_or_default().is_empty() {
            "?2 = ''"
        } else {
            "project_id = ?2"
        };
        let query = format!(
            "SELECT project_id, logical_trace_id, external_trace_id, revision, lifecycle, title,
                    service_name, environment, session_id, build_id, agent_id, identity_quality,
                    start_time_unix_nano, end_time_unix_nano, last_committed_unix_ms,
                    span_count, error_count, analysis_status, finding_count
             FROM logical_traces WHERE workspace_id = ?1
               AND {project_predicate}
               AND (?3 = '' OR environment = ?3)
               AND (?4 = '' OR build_id = ?4)
               AND (?5 = '' OR session_id = ?5)
               AND (?6 = '' OR service_name = ?6)
               AND (?7 = '' OR lifecycle = ?7)
               AND (?8 = '' OR identity_quality = ?8)
               AND (?9 = '' OR analysis_status = ?9)
               AND (?10 = '' OR instr(lower(title), lower(?10)) > 0
                    OR instr(lower(logical_trace_id), lower(?10)) > 0
                    OR instr(lower(COALESCE(service_name, '')), lower(?10)) > 0)
               AND (?11 IS NULL OR start_time_unix_nano >= ?11)
               AND (?12 IS NULL OR start_time_unix_nano <= ?12)
             ORDER BY {ordering}
             LIMIT ?13 OFFSET ?14"
        );
        let mut statement = control.prepare(&query)?;
        statement
            .query_map(
                params![
                    self.workspace_id,
                    scope.project_id.as_deref().unwrap_or(""),
                    scope.environment.as_deref().unwrap_or(""),
                    scope.build_id.as_deref().unwrap_or(""),
                    scope.session_id.as_deref().unwrap_or(""),
                    scope.service_name.as_deref().unwrap_or(""),
                    filters.lifecycle.map(TraceLifecycle::as_str).unwrap_or(""),
                    filters
                        .identity_quality
                        .map(IdentityQualityV1::as_str)
                        .unwrap_or(""),
                    filters
                        .analysis_status
                        .map(AnalysisStatus::as_str)
                        .unwrap_or(""),
                    filters.search.as_deref().unwrap_or(""),
                    scope.started_after_unix_nano.map(|value| value as i64),
                    scope.started_before_unix_nano.map(|value| value as i64),
                    limit as i64,
                    offset as i64
                ],
                map_run,
            )?
            .map(|row| row.map_err(StoreError::from))
            .collect()
    }

    pub fn run_count(&self) -> Result<u64, StoreError> {
        self.run_count_filtered(&RunFiltersV1::default())
    }

    pub fn run_count_filtered(&self, filters: &RunFiltersV1) -> Result<u64, StoreError> {
        filters.scope.validate().map_err(StoreError::Invalid)?;
        let scope = &filters.scope.criteria;
        let control = self.control.lock().expect("control store lock poisoned");
        let project_predicate = if scope.project_id.as_deref().unwrap_or_default().is_empty() {
            "?2 = ''"
        } else {
            "project_id = ?2"
        };
        let query = format!(
            "SELECT COUNT(*) FROM logical_traces WHERE workspace_id = ?1
               AND {project_predicate}
               AND (?3 = '' OR environment = ?3)
               AND (?4 = '' OR build_id = ?4)
               AND (?5 = '' OR session_id = ?5)
               AND (?6 = '' OR service_name = ?6)
               AND (?7 = '' OR lifecycle = ?7)
               AND (?8 = '' OR identity_quality = ?8)
               AND (?9 = '' OR analysis_status = ?9)
               AND (?10 = '' OR instr(lower(title), lower(?10)) > 0
                    OR instr(lower(logical_trace_id), lower(?10)) > 0
                    OR instr(lower(COALESCE(service_name, '')), lower(?10)) > 0)
               AND (?11 IS NULL OR start_time_unix_nano >= ?11)
               AND (?12 IS NULL OR start_time_unix_nano <= ?12)"
        );
        Ok(control.query_row(
            &query,
            params![
                self.workspace_id,
                scope.project_id.as_deref().unwrap_or(""),
                scope.environment.as_deref().unwrap_or(""),
                scope.build_id.as_deref().unwrap_or(""),
                scope.session_id.as_deref().unwrap_or(""),
                scope.service_name.as_deref().unwrap_or(""),
                filters.lifecycle.map(TraceLifecycle::as_str).unwrap_or(""),
                filters
                    .identity_quality
                    .map(IdentityQualityV1::as_str)
                    .unwrap_or(""),
                filters
                    .analysis_status
                    .map(AnalysisStatus::as_str)
                    .unwrap_or(""),
                filters.search.as_deref().unwrap_or(""),
                scope.started_after_unix_nano.map(|value| value as i64),
                scope.started_before_unix_nano.map(|value| value as i64),
            ],
            |row| row.get::<_, i64>(0),
        )? as u64)
    }

    pub fn lifecycle_counts(&self) -> Result<(u64, u64, u64, u64), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control
            .query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN lifecycle = 'live' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN lifecycle = 'quiescent' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN lifecycle = 'finalized' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN lifecycle = 'reopened' THEN 1 ELSE 0 END), 0)
                 FROM logical_traces WHERE workspace_id = ?1",
                [&self.workspace_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as u64,
                        row.get::<_, i64>(1)? as u64,
                        row.get::<_, i64>(2)? as u64,
                        row.get::<_, i64>(3)? as u64,
                    ))
                },
            )
            .map_err(StoreError::from)
    }

    pub fn source_totals(&self, source_id: &str) -> Result<(u64, u64), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let totals = control
            .query_row(
                "SELECT accepted_spans, rejected_spans FROM source_health
                 WHERE workspace_id = ?1 AND source_id = ?2",
                params![self.workspace_id, source_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )
            .optional()?;
        Ok(totals.unwrap_or_default())
    }

    pub fn get_run(&self, logical_trace_id: &str) -> Result<Option<RunSummary>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control
            .query_row(
                "SELECT project_id, logical_trace_id, external_trace_id, revision, lifecycle, title,
                        service_name, environment, session_id, build_id, agent_id, identity_quality,
                        start_time_unix_nano, end_time_unix_nano, last_committed_unix_ms,
                        span_count, error_count, analysis_status, finding_count
                 FROM logical_traces WHERE workspace_id = ?1 AND logical_trace_id = ?2",
                params![self.workspace_id, logical_trace_id],
                map_run,
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn list_spans(
        &self,
        logical_trace_id: &str,
        revision: u64,
        offset: u64,
        limit: u32,
        category: Option<&str>,
        errors_only: bool,
    ) -> Result<Vec<SpanRow>, StoreError> {
        self.list_spans_ordered(
            logical_trace_id,
            revision,
            offset,
            limit,
            category,
            errors_only,
            false,
        )
    }

    pub fn list_spans_timeline(
        &self,
        logical_trace_id: &str,
        revision: u64,
        offset: u64,
        limit: u32,
        category: Option<&str>,
        errors_only: bool,
    ) -> Result<Vec<SpanRow>, StoreError> {
        self.list_spans_ordered(
            logical_trace_id,
            revision,
            offset,
            limit,
            category,
            errors_only,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn list_spans_ordered(
        &self,
        logical_trace_id: &str,
        revision: u64,
        offset: u64,
        limit: u32,
        category: Option<&str>,
        errors_only: bool,
        chronological: bool,
    ) -> Result<Vec<SpanRow>, StoreError> {
        let analytics = self.analytics_reads.connection();
        let persisted_topology = has_persisted_topology(&analytics, logical_trace_id, revision)?;
        let mut query = String::from(
            "SELECT logical_trace_id, revision, span_id, parent_span_id, name, category,
                    start_time_unix_nano, duration_nano, status_code, status_message,
                    attributes_json, payload_refs_json, topology_depth, topology_has_children
             FROM spans WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE
             AND (?3 = '' OR category = ?3)",
        );
        if errors_only {
            query.push_str(" AND status_code = 2");
        }
        if chronological {
            query.push_str(" ORDER BY start_time_unix_nano, span_id LIMIT ?4 OFFSET ?5");
        } else {
            query.push_str(
                " ORDER BY topology_order NULLS LAST, start_time_unix_nano, span_id LIMIT ?4 OFFSET ?5",
            );
        }
        let mut statement = analytics.prepare(&query)?;
        let rows = statement.query_map(
            duck_params![
                logical_trace_id,
                revision as i64,
                category.unwrap_or(""),
                limit as i64,
                offset as i64
            ],
            |row| {
                let span_id: String = row.get(2)?;
                let attributes_json: String = row.get(10)?;
                let payload_refs_json: String = row.get(11)?;
                Ok(SpanRow {
                    logical_trace_id: row.get(0)?,
                    revision: row.get::<_, i64>(1)? as u64,
                    span_id: span_id.clone(),
                    parent_span_id: row.get(3)?,
                    name: row.get(4)?,
                    category: row.get(5)?,
                    start_time_unix_nano: row.get::<_, i64>(6)? as u64,
                    duration_nano: row.get::<_, i64>(7)? as u64,
                    status_code: row.get(8)?,
                    status_message: row.get(9)?,
                    depth: row
                        .get::<_, Option<i64>>(12)?
                        .map(|depth| depth as u32)
                        .unwrap_or_default(),
                    has_children: row.get::<_, Option<bool>>(13)?.unwrap_or(false),
                    attributes: persisted_json::decode_json_column(
                        &attributes_json,
                        10,
                        "span attributes",
                    )?,
                    payload_refs: persisted_json::decode_json_column(
                        &payload_refs_json,
                        11,
                        "span payload references",
                    )?,
                    events: Vec::new(),
                    links: Vec::new(),
                })
            },
        )?;
        let mut rows = rows
            .map(|row| row.map_err(StoreError::from))
            .collect::<Result<Vec<_>, _>>()?;
        if !persisted_topology {
            let span_ids = rows
                .iter()
                .map(|span| span.span_id.clone())
                .collect::<Vec<_>>();
            let annotations =
                self.live_topology_annotations(&analytics, logical_trace_id, revision, &span_ids)?;
            for (span, (depth, has_children)) in rows.iter_mut().zip(annotations) {
                span.depth = depth;
                span.has_children = has_children;
            }
        }
        Ok(rows)
    }

    pub fn get_span(
        &self,
        logical_trace_id: &str,
        revision: u64,
        span_id: &str,
    ) -> Result<Option<SpanRow>, StoreError> {
        let analytics = self.analytics_reads.connection();
        let mut span = analytics
            .query_row(
                "SELECT logical_trace_id, revision, span_id, parent_span_id, name, category,
                        start_time_unix_nano, duration_nano, status_code, status_message,
                        attributes_json, payload_refs_json, topology_depth, topology_has_children
                 FROM spans WHERE logical_trace_id = ?1 AND revision = ?2
                    AND span_id = ?3 AND is_current = TRUE",
                duck_params![logical_trace_id, revision as i64, span_id],
                |row| {
                    let span_id: String = row.get(2)?;
                    let attributes_json: String = row.get(10)?;
                    let payload_refs_json: String = row.get(11)?;
                    Ok(SpanRow {
                        logical_trace_id: row.get(0)?,
                        revision: row.get::<_, i64>(1)? as u64,
                        span_id: span_id.clone(),
                        parent_span_id: row.get(3)?,
                        name: row.get(4)?,
                        category: row.get(5)?,
                        start_time_unix_nano: row.get::<_, i64>(6)? as u64,
                        duration_nano: row.get::<_, i64>(7)? as u64,
                        status_code: row.get(8)?,
                        status_message: row.get(9)?,
                        depth: row
                            .get::<_, Option<i64>>(12)?
                            .map(|value| value as u32)
                            .unwrap_or_default(),
                        has_children: row.get::<_, Option<bool>>(13)?.unwrap_or(false),
                        attributes: persisted_json::decode_json_column(
                            &attributes_json,
                            10,
                            "span attributes",
                        )?,
                        payload_refs: persisted_json::decode_json_column(
                            &payload_refs_json,
                            11,
                            "span payload references",
                        )?,
                        events: Vec::new(),
                        links: Vec::new(),
                    })
                },
            )
            .optional()?;
        if let Some(row) = &mut span
            && !has_persisted_topology(&analytics, logical_trace_id, revision)?
        {
            let annotation = self.live_topology_annotations(
                &analytics,
                logical_trace_id,
                revision,
                std::slice::from_ref(&row.span_id),
            )?[0];
            row.depth = annotation.0;
            row.has_children = annotation.1;
        }
        if let Some(span) = &mut span {
            let mut events = analytics.prepare(
                "SELECT name, timestamp_unix_nano, attributes_json
                 FROM span_events WHERE logical_trace_id = ?1 AND revision = ?2 AND span_id = ?3
                   AND span_version = (
                       SELECT span_version FROM spans WHERE logical_trace_id = ?1 AND revision = ?2
                         AND span_id = ?3 AND is_current = TRUE
                   ) ORDER BY event_index",
            )?;
            span.events = events
                .query_map(
                    duck_params![logical_trace_id, revision as i64, span_id],
                    |row| {
                        let attributes: String = row.get(2)?;
                        Ok(crate::model::SpanEventV1 {
                            name: row.get(0)?,
                            timestamp_unix_nano: row.get::<_, i64>(1)? as u64,
                            attributes: persisted_json::decode_json_column(
                                &attributes,
                                2,
                                "span event attributes",
                            )?,
                            dropped_attributes_count: 0,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            let mut links = analytics.prepare(
                "SELECT linked_trace_id, linked_span_id, attributes_json
                 FROM span_links WHERE logical_trace_id = ?1 AND revision = ?2 AND span_id = ?3
                   AND span_version = (
                       SELECT span_version FROM spans WHERE logical_trace_id = ?1 AND revision = ?2
                         AND span_id = ?3 AND is_current = TRUE
                   ) ORDER BY link_index",
            )?;
            span.links = links
                .query_map(
                    duck_params![logical_trace_id, revision as i64, span_id],
                    |row| {
                        let attributes: String = row.get(2)?;
                        Ok(crate::model::SpanLinkV1 {
                            trace_id: row.get(0)?,
                            span_id: row.get(1)?,
                            trace_state: String::new(),
                            attributes: persisted_json::decode_json_column(
                                &attributes,
                                3,
                                "span link attributes",
                            )?,
                            dropped_attributes_count: 0,
                            flags: 0,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
        }
        Ok(span)
    }

    pub fn span_count(
        &self,
        logical_trace_id: &str,
        revision: u64,
        category: Option<&str>,
        errors_only: bool,
    ) -> Result<u64, StoreError> {
        let analytics = self.analytics_reads.connection();
        let mut query = String::from(
            "SELECT COUNT(*) FROM spans WHERE logical_trace_id = ?1 AND revision = ?2
             AND is_current = TRUE AND (?3 = '' OR category = ?3)",
        );
        if errors_only {
            query.push_str(" AND status_code = 2");
        }
        Ok(analytics.query_row(
            &query,
            duck_params![logical_trace_id, revision as i64, category.unwrap_or("")],
            |row| row.get::<_, i64>(0),
        )? as u64)
    }

    pub fn reveal_blob(&self, hash: &str, limit: usize) -> Result<Vec<u8>, StoreError> {
        match self.blobs.get(hash, limit) {
            Ok(bytes) => Ok(bytes),
            Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                let analytics = self.analytics_reads.connection();
                let compressed = analytics
                    .query_row(
                        "SELECT compressed FROM payload_blobs WHERE sha256 = ?1",
                        duck_params![hash],
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                    .optional()?;
                let compressed = compressed.ok_or_else(|| {
                    StoreError::Io(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!("blob {hash} does not exist"),
                    ))
                })?;
                let decoder = zstd::stream::Decoder::new(compressed.as_slice())?;
                let mut bytes = Vec::new();
                decoder.take(limit as u64).read_to_end(&mut bytes)?;
                Ok(bytes)
            }
            Err(error) => Err(error),
        }
    }

    pub fn latest_commit_sequence(&self) -> Result<u64, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        Ok(control.query_row(
            "SELECT COALESCE(MAX(commit_sequence), 0) FROM trace_delta_outbox",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64)
    }

    pub fn deltas_after(
        &self,
        sequence: u64,
        limit: usize,
    ) -> Result<Vec<TraceDeltaV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT delta_json FROM trace_delta_outbox WHERE commit_sequence > ?1
             ORDER BY commit_sequence LIMIT ?2",
        )?;
        statement
            .query_map(params![sequence as i64, limit as i64], |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }

    pub fn prune_deltas(&self, retain: usize) -> Result<(), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control.execute(
            "DELETE FROM trace_delta_outbox
             WHERE commit_sequence <= (SELECT COALESCE(MAX(commit_sequence), 0) FROM trace_delta_outbox) - ?1",
            params![retain as i64],
        )?;
        Ok(())
    }

    pub fn advance_lifecycle(
        &self,
        now_ms: i64,
        idle_ms: u64,
        grace_ms: u64,
    ) -> Result<Vec<TraceDeltaV1>, StoreError> {
        let mut changes = Vec::new();
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT logical_trace_id, lifecycle, last_committed_unix_ms
             FROM logical_traces WHERE workspace_id = ?1 AND lifecycle != 'finalized'",
        )?;
        let candidates = statement
            .query_map(params![self.workspace_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (trace_id, lifecycle, last_commit) in candidates {
            let age = now_ms.saturating_sub(last_commit) as u64;
            let target = if matches!(lifecycle.as_str(), "live" | "reopened") && age >= idle_ms {
                Some((TraceLifecycle::Quiescent, TraceChangeKind::Quiescent))
            } else if lifecycle == "quiescent" && age >= idle_ms.saturating_add(grace_ms) {
                Some((TraceLifecycle::Finalized, TraceChangeKind::Finalized))
            } else {
                None
            };
            if let Some((target, change)) = target {
                if target == TraceLifecycle::Finalized {
                    control.execute(
                        "UPDATE logical_traces SET lifecycle = ?1,
                            analysis_status = CASE WHEN analysis_status = 'reanalyzing' THEN 'reanalyzing' ELSE 'pending' END
                         WHERE logical_trace_id = ?2",
                        params![target.as_str(), trace_id],
                    )?;
                } else {
                    control.execute(
                        "UPDATE logical_traces SET lifecycle = ?1 WHERE logical_trace_id = ?2",
                        params![target.as_str(), trace_id],
                    )?;
                }
                control.execute(
                    "UPDATE trace_revisions SET lifecycle = ?1,
                        finalized_at_unix_ms = CASE WHEN ?1 = 'finalized' THEN ?2 ELSE finalized_at_unix_ms END,
                        topology_status = CASE WHEN ?1 = 'finalized' THEN 'pending' ELSE topology_status END,
                        topology_projection_version = CASE WHEN ?1 = 'finalized' THEN NULL ELSE topology_projection_version END,
                        topology_last_error = CASE WHEN ?1 = 'finalized' THEN NULL ELSE topology_last_error END,
                        topology_updated_at_unix_ms = CASE WHEN ?1 = 'finalized' THEN ?2 ELSE topology_updated_at_unix_ms END
                     WHERE logical_trace_id = ?3 AND revision = (SELECT revision FROM logical_traces WHERE logical_trace_id = ?3)",
                    params![target.as_str(), now_ms, trace_id],
                )?;
                let summary = query_run_locked(&control, &self.workspace_id, &trace_id)?
                    .ok_or_else(|| StoreError::Invalid("lifecycle trace disappeared".into()))?;
                changes.push(insert_delta_locked(
                    &control,
                    &self.workspace_id,
                    summary,
                    change,
                    Vec::new(),
                )?);
            }
        }
        Ok(changes)
    }

    pub fn journal_lag(&self) -> Result<u64, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        Ok(control.query_row(
            "SELECT COUNT(*) FROM ingest_journal WHERE projected = 0",
            [],
            |row| row.get::<_, i64>(0),
        )? as u64)
    }

    pub fn projection_backlog(&self) -> Result<(u64, Option<i64>), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control
            .query_row(
                "SELECT COUNT(*), MIN(received_at_unix_ms)
                 FROM ingest_journal WHERE projected = 0",
                [],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, Option<i64>>(1)?)),
            )
            .map_err(StoreError::from)
    }
}
