use super::*;

impl WorkspaceStore {
    fn load_active_failure_rows(
        &self,
        filters: &FailureFiltersV1,
        group_id: Option<&str>,
        finding_id: Option<&str>,
        offset: u64,
        limit: Option<u32>,
    ) -> Result<Vec<ActiveFailureRow>, StoreError> {
        let scope = &filters.scope.criteria;
        let severity = filters.severity.map(finding_severity_name);
        let recovery = filters.recovery.map(finding_recovery_name);
        let search = normalized_failure_search(filters.search.as_deref());
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT f.finding_json, f.logical_trace_id, f.revision, f.project_id,
                    f.run_title, f.service_name, t.analysis_status, f.telemetry_gaps_json,
                    f.presentation_json, f.analysis_id, f.group_id,
                    d.analysis_id, d.detector_id, d.detector_version,
                    d.state, d.updated_at_unix_ms,
                    CASE WHEN d.state IS NOT NULL AND (
                        d.analysis_id <> f.analysis_id OR
                        d.detector_id <> f.detector_id OR
                        d.detector_version <> f.detector_version
                    ) THEN 1 ELSE 0 END
               FROM active_failure_findings f
               JOIN logical_traces t ON t.logical_trace_id = f.logical_trace_id
          LEFT JOIN finding_dispositions d
                 ON d.workspace_id = t.workspace_id AND d.finding_id = f.finding_id
              WHERE t.workspace_id = ?1
                AND (?2 IS NULL OR f.project_id = ?2)
                AND (?3 IS NULL OR f.service_name = ?3)
                AND (?4 IS NULL OR f.environment = ?4)
                AND (?5 IS NULL OR f.build_id = ?5)
                AND (?6 IS NULL OR f.session_id = ?6)
                AND (?7 IS NULL OR f.run_started_at_unix_nano >= ?7)
                AND (?8 IS NULL OR f.run_started_at_unix_nano <= ?8)
                AND (?9 IS NULL OR f.severity = ?9)
                AND (?10 IS NULL OR f.recovery = ?10)
                AND (?11 IS NULL OR f.detector_id = ?11)
                AND (?12 IS NULL OR LOWER(
                    f.detector_id || ' ' || COALESCE(f.subject, '') || ' ' ||
                    COALESCE(f.operation, '')
                ) LIKE ?12)
                AND (?13 IS NULL OR f.group_id = ?13)
                AND (?14 IS NULL OR f.finding_id = ?14)
              ORDER BY CASE f.recovery WHEN 'unrecovered' THEN 0
                                      WHEN 'unknown' THEN 1 ELSE 2 END,
                       f.created_at DESC, f.finding_id
              LIMIT ?15 OFFSET ?16",
        )?;
        statement
            .query_map(
                params![
                    self.workspace_id,
                    scope.project_id,
                    scope.service_name,
                    scope.environment,
                    scope.build_id,
                    scope.session_id,
                    scope.started_after_unix_nano.map(|value| value as i64),
                    scope.started_before_unix_nano.map(|value| value as i64),
                    severity,
                    recovery,
                    filters.detector_id,
                    search,
                    group_id,
                    finding_id,
                    limit.map(i64::from).unwrap_or(-1),
                    offset as i64,
                ],
                |row| {
                    let status: String = row.get(6)?;
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        status,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<String>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, Option<i64>>(15)?,
                        row.get::<_, i64>(16)?,
                    ))
                },
            )?
            .map(|row| {
                let (
                    finding,
                    logical_trace_id,
                    revision,
                    project_id,
                    run_title,
                    service_name,
                    status,
                    gaps,
                    presentation,
                    analysis_id,
                    group_id,
                    disposition_analysis_id,
                    disposition_detector_id,
                    disposition_detector_version,
                    disposition_state,
                    disposition_updated_at,
                    disposition_stale,
                ) = row?;
                let finding: BehaviorFinding = serde_json::from_str(&finding)?;
                let (disposition, disposition_stale) = materialized_disposition(
                    &project_id,
                    &group_id,
                    &finding.finding_id,
                    disposition_analysis_id,
                    disposition_detector_id,
                    disposition_detector_version,
                    disposition_state,
                    disposition_updated_at,
                    disposition_stale != 0,
                )?;
                Ok(ActiveFailureRow {
                    finding,
                    logical_trace_id,
                    revision: revision as u64,
                    project_id,
                    run_title,
                    service_name,
                    analysis_status: AnalysisStatus::from_str(&status)
                        .unwrap_or(AnalysisStatus::Failed),
                    telemetry_gaps: serde_json::from_str(&gaps)?,
                    presentation: presentation
                        .map(|presentation| serde_json::from_str(&presentation))
                        .transpose()?,
                    analysis_id,
                    disposition,
                    disposition_stale,
                })
            })
            .collect()
    }

    pub fn failure_filter_options(&self) -> Result<(Vec<String>, Vec<String>), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let detectors = query_distinct_strings(
            &control,
            "SELECT DISTINCT f.detector_id
               FROM active_failure_findings f
               JOIN logical_traces t ON t.logical_trace_id = f.logical_trace_id
              WHERE t.workspace_id = ?1
              ORDER BY f.detector_id",
            &self.workspace_id,
        )?;
        let services = query_distinct_strings(
            &control,
            "SELECT DISTINCT t.service_name
               FROM active_failure_findings f
               JOIN logical_traces t ON t.logical_trace_id = f.logical_trace_id
              WHERE t.workspace_id = ?1 AND t.service_name IS NOT NULL
              ORDER BY t.service_name",
            &self.workspace_id,
        )?;
        Ok((detectors, services))
    }

    pub fn get_failure_group(
        &self,
        group_id: &str,
    ) -> Result<Option<FailureGroupDetail>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let project_id = control
            .query_row(
                "SELECT f.project_id
                   FROM active_failure_findings f
                   JOIN logical_traces t ON t.logical_trace_id = f.logical_trace_id
                  WHERE t.workspace_id = ?1 AND f.group_id = ?2
                  ORDER BY f.project_id
                  LIMIT 1",
                params![self.workspace_id, group_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        drop(control);
        let Some(project_id) = project_id else {
            return Ok(None);
        };
        self.get_failure_group_for_project(&project_id, group_id)
    }

    pub fn get_failure_group_for_project(
        &self,
        project_id: &str,
        group_id: &str,
    ) -> Result<Option<FailureGroupDetail>, StoreError> {
        self.get_failure_group_in_scope(&query_scope_for_project(project_id), group_id)
    }

    pub fn get_failure_group_in_scope(
        &self,
        scope: &QueryScopeV1,
        group_id: &str,
    ) -> Result<Option<FailureGroupDetail>, StoreError> {
        scope.validate().map_err(StoreError::Invalid)?;
        let filters = FailureFiltersV1 {
            scope: scope.clone(),
            include_fully_dismissed: true,
            ..FailureFiltersV1::default()
        };
        let (rows, _) = self.load_materialized_failure_groups(&filters, Some(group_id), 0, 1)?;
        let Some(summary) = self
            .hydrate_materialized_failure_groups(&filters, rows, true)?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let metadata = self.load_failure_group_metadata(scope, &summary.project_id, group_id)?;
        Ok(Some(FailureGroupDetail {
            summary,
            explanation:
                "Grouped because these findings have the same deterministic failure signature."
                    .into(),
            detector_versions: metadata.detector_versions,
            adapter_versions: metadata.adapter_versions,
            telemetry_gaps: metadata.telemetry_gaps,
        }))
    }

    fn load_failure_group_metadata(
        &self,
        scope: &QueryScopeV1,
        project_id: &str,
        group_id: &str,
    ) -> Result<FailureGroupMetadata, StoreError> {
        let criteria = &scope.criteria;
        let control = self.control.lock().expect("control store lock poisoned");
        let values = |expression: &str| -> Result<Vec<String>, StoreError> {
            let sql = format!(
                "SELECT DISTINCT {expression}
                   FROM active_failure_findings f
                  WHERE f.project_id = ?1 AND f.group_id = ?2
                    AND (?3 IS NULL OR f.service_name = ?3)
                    AND (?4 IS NULL OR f.environment = ?4)
                    AND (?5 IS NULL OR f.build_id = ?5)
                    AND (?6 IS NULL OR f.session_id = ?6)
                    AND (?7 IS NULL OR f.run_started_at_unix_nano >= ?7)
                    AND (?8 IS NULL OR f.run_started_at_unix_nano <= ?8)
                  ORDER BY 1"
            );
            let mut statement = control.prepare(&sql)?;
            statement
                .query_map(
                    params![
                        project_id,
                        group_id,
                        criteria.service_name,
                        criteria.environment,
                        criteria.build_id,
                        criteria.session_id,
                        criteria.started_after_unix_nano.map(|value| value as i64),
                        criteria.started_before_unix_nano.map(|value| value as i64),
                    ],
                    |row| row.get(0),
                )?
                .map(|row| row.map_err(StoreError::from))
                .collect()
        };
        let detector_versions = values("f.detector_id || '@' || f.detector_version")?;
        let adapter_versions = values("f.adapter_id || '@' || f.adapter_version")?;
        let mut diagnostics = control.prepare(
            "SELECT DISTINCT diagnostic.diagnostic
               FROM active_failure_diagnostics diagnostic
               JOIN active_failure_findings f ON f.finding_id = diagnostic.finding_id
              WHERE f.project_id = ?1 AND f.group_id = ?2
                AND (?3 IS NULL OR f.service_name = ?3)
                AND (?4 IS NULL OR f.environment = ?4)
                AND (?5 IS NULL OR f.build_id = ?5)
                AND (?6 IS NULL OR f.session_id = ?6)
                AND (?7 IS NULL OR f.run_started_at_unix_nano >= ?7)
                AND (?8 IS NULL OR f.run_started_at_unix_nano <= ?8)
              ORDER BY diagnostic.diagnostic",
        )?;
        let telemetry_gaps = diagnostics
            .query_map(
                params![
                    project_id,
                    group_id,
                    criteria.service_name,
                    criteria.environment,
                    criteria.build_id,
                    criteria.session_id,
                    criteria.started_after_unix_nano.map(|value| value as i64),
                    criteria.started_before_unix_nano.map(|value| value as i64),
                ],
                |row| row.get::<_, String>(0),
            )?
            .map(|row| row.map_err(StoreError::from))
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(FailureGroupMetadata {
            detector_versions,
            adapter_versions,
            telemetry_gaps,
        })
    }

    pub fn list_failure_occurrences(
        &self,
        group_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<FailureOccurrence>, StoreError> {
        let Some(detail) = self.get_failure_group(group_id)? else {
            return Ok(Vec::new());
        };
        self.list_failure_occurrences_in_scope(&detail.summary.scope, group_id, offset, limit)
    }

    pub fn list_failure_occurrences_for_project(
        &self,
        project_id: &str,
        group_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<FailureOccurrence>, StoreError> {
        self.list_failure_occurrences_in_scope(
            &query_scope_for_project(project_id),
            group_id,
            offset,
            limit,
        )
    }

    pub fn list_failure_occurrences_in_scope(
        &self,
        scope: &QueryScopeV1,
        group_id: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<FailureOccurrence>, StoreError> {
        scope.validate().map_err(StoreError::Invalid)?;
        let filters = FailureFiltersV1 {
            scope: scope.clone(),
            ..FailureFiltersV1::default()
        };
        let occurrences = self
            .load_active_failure_rows(&filters, Some(group_id), None, offset, Some(limit))?
            .into_iter()
            .map(|row| failure_occurrence_from_row(scope, group_id, row))
            .collect::<Vec<_>>();
        Ok(occurrences)
    }

    pub fn get_finding_evidence(
        &self,
        group_id: &str,
        finding_id: &str,
        maximum_spans: usize,
    ) -> Result<Option<FindingEvidence>, StoreError> {
        let filters = FailureFiltersV1::default();
        let Some(row) = self
            .load_active_failure_rows(&filters, Some(group_id), Some(finding_id), 0, Some(1))?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        self.get_finding_evidence_for_project(&row.project_id, group_id, finding_id, maximum_spans)
    }

    pub fn get_finding_evidence_for_project(
        &self,
        project_id: &str,
        group_id: &str,
        finding_id: &str,
        maximum_spans: usize,
    ) -> Result<Option<FindingEvidence>, StoreError> {
        self.get_finding_evidence_in_scope(
            &query_scope_for_project(project_id),
            group_id,
            finding_id,
            maximum_spans,
        )
    }

    pub fn get_finding_evidence_in_scope(
        &self,
        scope: &QueryScopeV1,
        group_id: &str,
        finding_id: &str,
        maximum_spans: usize,
    ) -> Result<Option<FindingEvidence>, StoreError> {
        scope.validate().map_err(StoreError::Invalid)?;
        let filters = FailureFiltersV1 {
            scope: scope.clone(),
            ..FailureFiltersV1::default()
        };
        let Some(row) = self
            .load_active_failure_rows(&filters, Some(group_id), Some(finding_id), 0, Some(1))?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let presentation = row.presentation.clone();
        let analysis_id = row.analysis_id.clone();
        let occurrence = failure_occurrence_from_row(scope, group_id, row);
        let behavior_json = {
            let control = self.control.lock().expect("control store lock poisoned");
            control
                .query_row(
                    "SELECT behavior_json FROM analysis_runs WHERE analysis_id = ?1",
                    params![analysis_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        }
        .ok_or_else(|| StoreError::Invalid("finding analysis disappeared".into()))?;
        let behavior: AgentBehaviorTrace = serde_json::from_str(&behavior_json)?;
        let mut wanted = {
            let control = self.control.lock().expect("control store lock poisoned");
            let mut statement = control.prepare(
                "SELECT span_id
                   FROM active_failure_evidence_refs
                  WHERE finding_id = ?1 AND span_id IS NOT NULL
                  ORDER BY evidence_index",
            )?;
            statement
                .query_map(params![finding_id], |row| row.get::<_, String>(0))?
                .collect::<Result<BTreeSet<_>, _>>()?
        };
        wanted.extend(
            behavior
                .final_outcome
                .evidence
                .iter()
                .filter_map(|evidence| evidence.span_id.clone()),
        );
        let evidence_span_ids = wanted.iter().cloned().collect::<Vec<_>>();
        let mut spans = Vec::new();
        let mut cursor = wanted.into_iter().collect::<Vec<_>>();
        while let Some(span_id) = cursor.pop() {
            if spans.len() >= maximum_spans
                || spans.iter().any(|span: &SpanRow| span.span_id == span_id)
            {
                continue;
            }
            if let Some(span) =
                self.get_span(&occurrence.logical_trace_id, occurrence.revision, &span_id)?
            {
                if let Some(parent) = span.parent_span_id.clone() {
                    cursor.push(parent);
                }
                spans.push(span);
            }
        }
        if spans.len() < maximum_spans {
            let center = spans
                .iter()
                .map(|span| u128::from(span.start_time_unix_nano))
                .sum::<u128>()
                .checked_div(spans.len() as u128)
                .unwrap_or_default() as u64;
            for span_id in self.nearby_tool_span_ids(
                &occurrence.logical_trace_id,
                occurrence.revision,
                center,
                16.min(maximum_spans.saturating_sub(spans.len())),
            )? {
                cursor.push(span_id);
            }
            while let Some(span_id) = cursor.pop() {
                if spans.len() >= maximum_spans
                    || spans.iter().any(|span: &SpanRow| span.span_id == span_id)
                {
                    continue;
                }
                if let Some(span) =
                    self.get_span(&occurrence.logical_trace_id, occurrence.revision, &span_id)?
                {
                    if let Some(parent) = span.parent_span_id.clone() {
                        cursor.push(parent);
                    }
                    spans.push(span);
                }
            }
        }
        spans.sort_by_key(|span| (span.start_time_unix_nano, span.span_id.clone()));
        let candidate = self.load_candidate(finding_id)?;
        Ok(Some(FindingEvidence {
            occurrence,
            presentation,
            spans,
            evidence_span_ids,
            final_outcome: serde_json::to_value(&behavior.final_outcome)?,
            candidate,
        }))
    }

    fn nearby_tool_span_ids(
        &self,
        logical_trace_id: &str,
        revision: u64,
        center_unix_nano: u64,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let analytics = self.analytics_reads.connection();
        let mut statement = analytics.prepare(
            "SELECT span_id FROM spans
             WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE
               AND lower(category) = 'tool'
             ORDER BY abs(start_time_unix_nano - ?3), start_time_unix_nano, span_id
             LIMIT ?4",
        )?;
        statement
            .query_map(
                duck_params![
                    logical_trace_id,
                    revision as i64,
                    center_unix_nano as i64,
                    limit as i64
                ],
                |row| row.get(0),
            )?
            .map(|row| row.map_err(StoreError::from))
            .collect()
    }
}
