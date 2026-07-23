use super::*;

mod cohorts;

impl WorkspaceStore {
    pub(in crate::workspace) fn backfill_active_failure_projection(
        &self,
    ) -> Result<(), StoreError> {
        let mut control = self.control.lock().expect("control store lock poisoned");
        let pending = {
            let mut statement = control.prepare(
                "SELECT r.analysis_id, r.logical_trace_id, r.revision,
                        r.adapter_id, r.adapter_version, r.behavior_json, r.findings_json
                   FROM active_analysis_runs active
                   JOIN analysis_runs r ON r.analysis_id = active.analysis_id
              LEFT JOIN active_failure_projection_state projected
                     ON projected.logical_trace_id = active.logical_trace_id
                  WHERE projected.analysis_id IS NULL
                     OR projected.analysis_id <> active.analysis_id
                     OR projected.projection_schema_version <> ?1
                     OR EXISTS (
                         SELECT 1 FROM active_failure_findings finding
                          WHERE finding.logical_trace_id = active.logical_trace_id
                            AND finding.projection_schema_version <> ?1
                     )
                     OR (
                         SELECT COUNT(DISTINCT finding.group_id)
                           FROM active_failure_findings finding
                          WHERE finding.logical_trace_id = active.logical_trace_id
                     ) <> (
                         SELECT COUNT(*)
                           FROM active_failure_group_memberships membership
                          WHERE membership.logical_trace_id = active.logical_trace_id
                            AND membership.projection_schema_version = ?1
                     )",
            )?;
            statement
                .query_map(params![ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? as u64,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        if pending.is_empty() {
            return Ok(());
        }
        let transaction = control.transaction()?;
        for (analysis_id, trace_id, revision, adapter_id, adapter_version, behavior, findings) in
            pending
        {
            let behavior: AgentBehaviorTrace = serde_json::from_str(&behavior)?;
            let findings: Vec<BehaviorFinding> = serde_json::from_str(&findings)?;
            replace_active_failure_projection(
                &transaction,
                &analysis_id,
                &trace_id,
                revision,
                &adapter_id,
                &adapter_version,
                &behavior,
                &findings,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Atomically marks finalized active results stale when their complete
    /// analysis implementation identity differs from `expected`. The old
    /// immutable result remains active and queryable until `commit_analysis`
    /// switches the active pointer to its replacement.
    pub fn enqueue_stale_analyses(
        &self,
        expected: &AnalysisDefinitionV1,
    ) -> Result<Vec<TraceDeltaV1>, StoreError> {
        expected.validate().map_err(StoreError::Invalid)?;
        let detector_versions_json = serde_json::to_string(&expected.detector_versions)?;
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let stale = {
            let mut statement = transaction.prepare(
                "SELECT t.logical_trace_id
                   FROM logical_traces t
                   JOIN active_analysis_runs active
                     ON active.logical_trace_id = t.logical_trace_id
                    AND active.revision = t.revision
                   JOIN analysis_runs analysis ON analysis.analysis_id = active.analysis_id
                  WHERE t.workspace_id = ?1
                    AND t.lifecycle = 'finalized'
                    AND t.analysis_status IN ('ready', 'failed')
                    AND (
                        analysis.input_schema_version <> ?2
                        OR analysis.projection_version <> ?3
                        OR analysis.adapter_id <> ?4
                        OR analysis.adapter_version <> ?5
                        OR analysis.detector_profile_id <> ?6
                        OR analysis.detector_profile_version <> ?7
                        OR analysis.detector_versions_json <> ?8
                        OR analysis.grouping_version <> ?9
                        OR analysis.risk_model_version <> ?10
                    )
                  ORDER BY t.logical_trace_id",
            )?;
            statement
                .query_map(
                    params![
                        self.workspace_id,
                        expected.input_schema_version,
                        expected.projection_version,
                        expected.adapter_id,
                        expected.adapter_version,
                        expected.detector_profile_id,
                        expected.detector_profile_version,
                        detector_versions_json,
                        expected.grouping_version,
                        expected.risk_model_version,
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut deltas = Vec::with_capacity(stale.len());
        for logical_trace_id in stale {
            let changed = transaction.execute(
                "UPDATE logical_traces SET analysis_status = 'reanalyzing'
                  WHERE workspace_id = ?1 AND logical_trace_id = ?2
                    AND lifecycle = 'finalized' AND analysis_status IN ('ready', 'failed')",
                params![self.workspace_id, logical_trace_id],
            )?;
            if changed == 0 {
                continue;
            }
            let summary =
                query_run_transaction(&transaction, &self.workspace_id, &logical_trace_id)?
                    .ok_or_else(|| {
                        StoreError::Invalid("stale analysis trace disappeared".into())
                    })?;
            deltas.push(insert_delta_transaction(
                &transaction,
                &self.workspace_id,
                summary,
                TraceChangeKind::Reanalyzing,
                Vec::new(),
            )?);
        }
        transaction.commit()?;
        Ok(deltas)
    }

    pub fn pending_analysis_requests(
        &self,
        limit: usize,
    ) -> Result<Vec<AnalysisRequestV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT logical_trace_id, revision, analysis_status
             FROM logical_traces
             WHERE workspace_id = ?1 AND lifecycle = 'finalized'
               AND analysis_status IN ('pending', 'reanalyzing')
             ORDER BY last_committed_unix_ms, logical_trace_id LIMIT ?2",
        )?;
        statement
            .query_map(params![self.workspace_id, limit as i64], |row| {
                let status: String = row.get(2)?;
                Ok(AnalysisRequestV1 {
                    logical_trace_id: row.get(0)?,
                    revision: row.get::<_, i64>(1)? as u64,
                    reanalysis: status == "reanalyzing",
                })
            })?
            .map(|row| row.map_err(StoreError::from))
            .collect()
    }

    pub fn analysis_counts(&self) -> Result<(u64, u64), StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control
            .query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN analysis_status IN ('pending', 'reanalyzing') THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN analysis_status = 'analyzing' THEN 1 ELSE 0 END), 0)
                 FROM logical_traces WHERE workspace_id = ?1 AND lifecycle = 'finalized'",
                params![self.workspace_id],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )
            .map_err(StoreError::from)
    }

    pub fn active_findings(&self) -> Result<Vec<BehaviorFinding>, StoreError> {
        Ok(self
            .load_active_analyses()?
            .into_iter()
            .flat_map(|analysis| analysis.findings)
            .collect())
    }

    pub fn active_detector_versions(
        &self,
    ) -> Result<BTreeMap<String, BTreeSet<String>>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT a.detector_versions_json
               FROM active_analysis_runs active
               JOIN analysis_runs a ON a.analysis_id = active.analysis_id",
        )?;
        let mut versions = BTreeMap::<String, BTreeSet<String>>::new();
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        for encoded in rows {
            let active = serde_json::from_str::<BTreeMap<String, String>>(&encoded?)?;
            for (detector_id, version) in active {
                versions.entry(detector_id).or_default().insert(version);
            }
        }
        Ok(versions)
    }

    pub fn mark_analysis_started(
        &self,
        request: &AnalysisRequestV1,
    ) -> Result<Option<TraceDeltaV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let changed = control.execute(
            "UPDATE logical_traces
             SET analysis_status = CASE WHEN analysis_status = 'reanalyzing' THEN 'reanalyzing' ELSE 'analyzing' END
             WHERE workspace_id = ?1 AND logical_trace_id = ?2 AND revision = ?3
               AND lifecycle = 'finalized' AND analysis_status IN ('pending', 'reanalyzing')",
            params![self.workspace_id, request.logical_trace_id, request.revision as i64],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let summary = query_run_locked(&control, &self.workspace_id, &request.logical_trace_id)?
            .ok_or_else(|| StoreError::Invalid("analysis trace disappeared".into()))?;
        Ok(Some(insert_delta_locked(
            &control,
            &self.workspace_id,
            summary,
            if request.reanalysis {
                TraceChangeKind::Reanalyzing
            } else {
                TraceChangeKind::Analyzing
            },
            Vec::new(),
        )?))
    }

    pub fn load_analysis_trace(
        &self,
        logical_trace_id: &str,
        revision: u64,
    ) -> Result<Trace, StoreError> {
        Ok(self.load_behavior_input(logical_trace_id, revision)?.trace)
    }

    pub fn load_behavior_input(
        &self,
        logical_trace_id: &str,
        revision: u64,
    ) -> Result<BehaviorInputV1, StoreError> {
        let analytics = self.analytics_reads.connection();
        let mut statement = analytics.prepare(
            "SELECT span_id, parent_span_id, name, category, status_code, status_message,
                    attributes_json, COALESCE(payload_identities_json, '{}'),
                    start_time_unix_nano, end_time_unix_nano,
                    COALESCE(source_id, 'unknown'), COALESCE(decoder_version, 'unknown'),
                    COALESCE(semantic_mapping_version, 'unknown')
             FROM spans WHERE logical_trace_id = ?1 AND revision = ?2 AND is_current = TRUE
             ORDER BY start_time_unix_nano, span_id",
        )?;
        let rows = statement.query_map(duck_params![logical_trace_id, revision as i64], |row| {
            let category: String = row.get(3)?;
            let status_code: i32 = row.get(4)?;
            let attributes_json: String = row.get(6)?;
            let payload_identities_json: String = row.get(7)?;
            let started_at = row.get::<_, i64>(8)? as u64;
            let ended_at = row.get::<_, i64>(9)? as u64;
            let payload_identities = serde_json::from_str::<BTreeMap<String, PayloadIdentityV1>>(
                &payload_identities_json,
            )
            .unwrap_or_default()
            .into_iter()
            .filter(|(key, _)| is_analysis_payload_key(key))
            .map(|(key, identity)| {
                let quality = match identity.quality {
                    PayloadIdentityQualityV1::Explicit => FactQuality::Explicit,
                    PayloadIdentityQualityV1::Derived => FactQuality::Derived,
                    PayloadIdentityQualityV1::Unknown => FactQuality::Missing,
                };
                (
                    key,
                    PayloadIdentity {
                        fingerprint: identity.fingerprint,
                        blob_id: identity.blob.map(|blob| blob.sha256),
                        original_bytes: identity.original_bytes,
                        quality,
                    },
                )
            })
            .collect();
            Ok(Span {
                id: row.get(0)?,
                trace_id: Some(logical_trace_id.to_string()),
                parent_id: row.get(1)?,
                name: row.get(2)?,
                kind: trace_span_kind(&category),
                input: None,
                output: None,
                error: None,
                started_at: None,
                ended_at: None,
                source_status: source_span_status(status_code),
                start_time_unix_nano: Some(started_at),
                end_time_unix_nano: Some(ended_at),
                duration_nano: Some(ended_at.saturating_sub(started_at)),
                events: Vec::new(),
                links: Vec::new(),
                payload_identities,
                provenance: Some(SpanProvenance {
                    source_id: row.get(10)?,
                    decoder_version: row.get(11)?,
                    semantic_mapping_version: row.get(12)?,
                }),
                attributes: safe_analysis_attributes(&persisted_json::decode_json_column(
                    &attributes_json,
                    6,
                    "analysis span attributes",
                )?),
            })
        })?;
        let mut spans = rows.collect::<Result<Vec<_>, _>>()?;
        let span_indices = spans
            .iter()
            .enumerate()
            .map(|(index, span)| (span.id.clone(), index))
            .collect::<HashMap<_, _>>();

        let mut event_statement = analytics.prepare(
            "SELECT e.span_id, e.name, e.timestamp_unix_nano, e.attributes_json,
                    e.evidence_identity
               FROM span_events e
               JOIN spans s ON s.logical_trace_id = e.logical_trace_id
                 AND s.revision = e.revision AND s.span_id = e.span_id
                 AND s.span_version = e.span_version AND s.is_current = TRUE
              WHERE e.logical_trace_id = ?1 AND e.revision = ?2
              ORDER BY e.span_id, e.event_index",
        )?;
        let events =
            event_statement.query_map(duck_params![logical_trace_id, revision as i64], |row| {
                let attributes_json: String = row.get(3)?;
                Ok((
                    row.get::<_, String>(0)?,
                    SpanEvent {
                        name: row.get(1)?,
                        timestamp_unix_nano: row.get::<_, i64>(2)? as u64,
                        attributes: safe_analysis_attributes(&persisted_json::decode_json_column(
                            &attributes_json,
                            3,
                            "analysis span event attributes",
                        )?),
                        identity: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    },
                ))
            })?;
        for event in events {
            let (span_id, event) = event?;
            if let Some(index) = span_indices.get(&span_id) {
                spans[*index].events.push(event);
            }
        }

        let mut link_statement = analytics.prepare(
            "SELECT l.span_id, l.linked_trace_id, l.linked_span_id, COALESCE(l.trace_state, ''),
                    l.attributes_json, l.evidence_identity
               FROM span_links l
               JOIN spans s ON s.logical_trace_id = l.logical_trace_id
                 AND s.revision = l.revision AND s.span_id = l.span_id
                 AND s.span_version = l.span_version AND s.is_current = TRUE
              WHERE l.logical_trace_id = ?1 AND l.revision = ?2
              ORDER BY l.span_id, l.link_index",
        )?;
        let links =
            link_statement.query_map(duck_params![logical_trace_id, revision as i64], |row| {
                let attributes_json: String = row.get(4)?;
                Ok((
                    row.get::<_, String>(0)?,
                    SpanLink {
                        trace_id: row.get(1)?,
                        span_id: row.get(2)?,
                        trace_state: row.get(3)?,
                        attributes: safe_analysis_attributes(&persisted_json::decode_json_column(
                            &attributes_json,
                            4,
                            "analysis span link attributes",
                        )?),
                        identity: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    },
                ))
            })?;
        for link in links {
            let (span_id, link) = link?;
            if let Some(index) = span_indices.get(&span_id) {
                spans[*index].links.push(link);
            }
        }

        let mut source_ids = BTreeSet::new();
        let mut decoder_versions = BTreeSet::new();
        let mut semantic_mapping_versions = BTreeSet::new();
        for span in &spans {
            if let Some(provenance) = &span.provenance {
                source_ids.insert(provenance.source_id.clone());
                decoder_versions.insert(provenance.decoder_version.clone());
                semantic_mapping_versions.insert(provenance.semantic_mapping_version.clone());
            }
        }
        let mut trace = Trace::new(logical_trace_id);
        trace.spans = spans;
        trace
            .metadata
            .insert("perseval.revision".into(), Value::from(revision));
        let source_id = if source_ids.len() == 1 {
            source_ids.into_iter().next().unwrap_or_default()
        } else {
            let material = source_ids.into_iter().collect::<Vec<_>>().join("\0");
            format!("multiple:sha256:{}", hex::encode(Sha256::digest(material)))
        };
        BehaviorInputV1::safe(
            trace,
            BehaviorInputProvenanceV1 {
                projection_version: SAFE_BEHAVIOR_PROJECTION_VERSION.into(),
                source_id,
                decoder_versions,
                semantic_mapping_versions,
            },
        )
        .map_err(|error| StoreError::Invalid(error.to_string()))
    }

    pub fn commit_analysis(&self, result: &AnalysisResultV1) -> Result<TraceDeltaV1, StoreError> {
        if result.schema_version != ANALYSIS_RESULT_SCHEMA_VERSION {
            return Err(StoreError::Invalid(format!(
                "unsupported analysis result schema {}",
                result.schema_version
            )));
        }
        if result.identity.schema_version != ANALYSIS_IDENTITY_SCHEMA_VERSION
            || result.analysis_id != result.identity.analysis_id()
            || result.identity.logical_trace_id != result.logical_trace_id
            || result.identity.revision != result.revision
            || result.identity.adapter_id != result.adapter_id
            || result.identity.adapter_version != result.adapter_version
            || result.identity.grouping_version.trim().is_empty()
            || result.identity.risk_model_version.trim().is_empty()
        {
            return Err(StoreError::Invalid(
                "analysis identity does not match the immutable result".into(),
            ));
        }
        if result.detection_report.findings != result.findings {
            return Err(StoreError::Invalid(
                "analysis findings differ from the detection report".into(),
            ));
        }
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let current_revision = transaction.query_row(
            "SELECT revision FROM logical_traces WHERE workspace_id = ?1 AND logical_trace_id = ?2",
            params![self.workspace_id, result.logical_trace_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        if current_revision != result.revision {
            return Err(StoreError::Invalid("stale analysis result".into()));
        }
        let identity_json = serde_json::to_string(&result.identity)?;
        let detector_versions_json = serde_json::to_string(&result.identity.detector_versions)?;
        let behavior_json = serde_json::to_string(&result.behavior)?;
        let detection_report_json = serde_json::to_string(&result.detection_report)?;
        let findings_json = serde_json::to_string(&result.findings)?;
        if let Some((persisted_identity, persisted_behavior, persisted_report, persisted_findings)) =
            transaction
                .query_row(
                    "SELECT identity_json, behavior_json, detection_report_json, findings_json
                       FROM analysis_runs WHERE analysis_id = ?1",
                    params![result.analysis_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .optional()?
            && (persisted_identity != identity_json
                || persisted_behavior != behavior_json
                || persisted_report != detection_report_json
                || persisted_findings != findings_json)
        {
            return Err(StoreError::Invalid(
                "immutable analysis identity already contains different content".into(),
            ));
        }
        transaction.execute(
            "INSERT OR IGNORE INTO analysis_runs(
                analysis_id, logical_trace_id, revision, identity_json,
                input_schema_version, projection_version, adapter_id, adapter_version,
                detector_profile_id, detector_profile_version, detector_versions_json,
                grouping_version, risk_model_version, behavior_json, detection_report_json,
                findings_json, committed_at_unix_ms, error
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, NULL
             )",
            params![
                result.analysis_id,
                result.logical_trace_id,
                result.revision as i64,
                identity_json,
                result.identity.input_schema_version,
                result.identity.projection_version,
                result.adapter_id,
                result.adapter_version,
                result.identity.detector_profile_id,
                result.identity.detector_profile_version,
                detector_versions_json,
                result.identity.grouping_version,
                result.identity.risk_model_version,
                behavior_json,
                detection_report_json,
                findings_json,
                now_unix_ms(),
            ],
        )?;
        transaction.execute(
            "INSERT INTO active_analysis_runs(
                logical_trace_id, revision, analysis_id, activated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(logical_trace_id) DO UPDATE SET
                revision = excluded.revision,
                analysis_id = excluded.analysis_id,
                activated_at_unix_ms = excluded.activated_at_unix_ms",
            params![
                result.logical_trace_id,
                result.revision as i64,
                result.analysis_id,
                now_unix_ms(),
            ],
        )?;
        replace_active_failure_projection(
            &transaction,
            &result.analysis_id,
            &result.logical_trace_id,
            result.revision,
            &result.adapter_id,
            &result.adapter_version,
            &result.behavior,
            &result.findings,
        )?;
        transaction.execute(
            "UPDATE logical_traces SET analysis_status = 'ready', finding_count = ?1
             WHERE workspace_id = ?2 AND logical_trace_id = ?3 AND revision = ?4",
            params![
                result.findings.len() as i64,
                self.workspace_id,
                result.logical_trace_id,
                result.revision as i64
            ],
        )?;
        let summary =
            query_run_transaction(&transaction, &self.workspace_id, &result.logical_trace_id)?
                .ok_or_else(|| StoreError::Invalid("analyzed trace disappeared".into()))?;
        let delta = insert_delta_transaction(
            &transaction,
            &self.workspace_id,
            summary,
            TraceChangeKind::FindingsCommitted,
            Vec::new(),
        )?;
        transaction.commit()?;
        Ok(delta)
    }

    pub fn fail_analysis(
        &self,
        request: &AnalysisRequestV1,
        message: &str,
    ) -> Result<Option<TraceDeltaV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let changed = control.execute(
            "UPDATE logical_traces SET analysis_status = 'failed'
             WHERE workspace_id = ?1 AND logical_trace_id = ?2 AND revision = ?3",
            params![
                self.workspace_id,
                request.logical_trace_id,
                request.revision as i64
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        control.execute(
            "INSERT INTO analysis_failures(
                logical_trace_id, revision, error, failed_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                request.logical_trace_id,
                request.revision as i64,
                message,
                now_unix_ms(),
            ],
        )?;
        let summary = query_run_locked(&control, &self.workspace_id, &request.logical_trace_id)?
            .ok_or_else(|| StoreError::Invalid("failed analysis trace disappeared".into()))?;
        Ok(Some(insert_delta_locked(
            &control,
            &self.workspace_id,
            summary,
            TraceChangeKind::AnalysisFailed,
            Vec::new(),
        )?))
    }
}

#[allow(clippy::too_many_arguments)]
fn replace_active_failure_projection(
    transaction: &rusqlite::Transaction<'_>,
    analysis_id: &str,
    logical_trace_id: &str,
    revision: u64,
    adapter_id: &str,
    adapter_version: &str,
    behavior: &AgentBehaviorTrace,
    findings: &[BehaviorFinding],
) -> Result<(), StoreError> {
    transaction.execute(
        "DELETE FROM active_failure_evidence_refs WHERE logical_trace_id = ?1",
        params![logical_trace_id],
    )?;
    transaction.execute(
        "DELETE FROM active_failure_diagnostics WHERE logical_trace_id = ?1",
        params![logical_trace_id],
    )?;
    transaction.execute(
        "DELETE FROM active_failure_findings WHERE logical_trace_id = ?1",
        params![logical_trace_id],
    )?;
    transaction.execute(
        "DELETE FROM active_failure_group_memberships WHERE logical_trace_id = ?1",
        params![logical_trace_id],
    )?;
    transaction.execute(
        "DELETE FROM active_failure_group_detectors WHERE logical_trace_id = ?1",
        params![logical_trace_id],
    )?;
    let trace_metadata = transaction.query_row(
        "SELECT project_id, service_name, environment, build_id, session_id, title,
                start_time_unix_nano
           FROM logical_traces WHERE logical_trace_id = ?1",
        params![logical_trace_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        },
    )?;
    let (project_id, service_name, environment, build_id, session_id, run_title, run_started_at) =
        trace_metadata;
    let groups = KnownSignatureGrouper.group(findings);
    let group_ids = groups
        .iter()
        .map(|group| (group.failure_signature.clone(), group.group_id.clone()))
        .collect::<HashMap<_, _>>();
    let telemetry_gaps = EvidencePacketBuilder
        .build(std::slice::from_ref(behavior), &[])
        .telemetry_gaps;
    let telemetry_gaps_json = serde_json::to_string(&telemetry_gaps)?;
    for finding in findings {
        let presentation = FindingPresenter.present(finding);
        let subject = finding.metadata.get("subject").and_then(Value::as_str);
        let operation = finding.metadata.get("operation").and_then(Value::as_str);
        transaction.execute(
            "INSERT INTO active_failure_findings(
                finding_id, projection_schema_version, logical_trace_id, revision,
                project_id, service_name, environment, build_id, session_id,
                run_title, run_started_at_unix_nano, analysis_id,
                failure_signature, group_id, detector_id, detector_version,
                severity, recovery, subject, operation, created_at,
                finding_json, presentation_json, telemetry_gaps_json,
                adapter_id, adapter_version
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                ?21, ?22, ?23, ?24, ?25, ?26
             )",
            params![
                finding.finding_id,
                ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION,
                logical_trace_id,
                revision as i64,
                project_id,
                service_name,
                environment,
                build_id,
                session_id,
                run_title,
                run_started_at,
                analysis_id,
                finding.failure_signature,
                group_ids
                    .get(&finding.failure_signature)
                    .expect("every finding belongs to a projected exact group"),
                finding.detector_id,
                finding.detector_version,
                finding_severity_name(finding.severity),
                finding_recovery_name(finding.recovery),
                subject,
                operation,
                finding.created_at,
                serde_json::to_string(finding)?,
                presentation
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                telemetry_gaps_json,
                adapter_id,
                adapter_version,
            ],
        )?;
        if let Some(presentation) = &presentation {
            for (index, presented) in presentation.evidence.iter().enumerate() {
                let role = serde_json::to_string(&presented.role)?
                    .trim_matches('"')
                    .to_owned();
                transaction.execute(
                    "INSERT INTO active_failure_evidence_refs(
                        finding_id, evidence_index, analysis_id, logical_trace_id, revision,
                        evidence_kind, evidence_identity, span_id, role, explanation
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        finding.finding_id,
                        index as i64,
                        analysis_id,
                        logical_trace_id,
                        revision as i64,
                        presented.evidence.kind,
                        presented.evidence.identity,
                        presented.evidence.span_id,
                        role,
                        presented.explanation,
                    ],
                )?;
            }
        } else {
            for (index, evidence) in finding.evidence.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO active_failure_evidence_refs(
                        finding_id, evidence_index, analysis_id, logical_trace_id, revision,
                        evidence_kind, evidence_identity, span_id, role, explanation
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'context', '')",
                    params![
                        finding.finding_id,
                        index as i64,
                        analysis_id,
                        logical_trace_id,
                        revision as i64,
                        evidence.kind,
                        evidence.identity,
                        evidence.span_id,
                    ],
                )?;
            }
        }
        for (index, diagnostic) in telemetry_gaps.iter().enumerate() {
            transaction.execute(
                "INSERT INTO active_failure_diagnostics(
                    finding_id, diagnostic_index, analysis_id, logical_trace_id,
                    revision, diagnostic
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    finding.finding_id,
                    index as i64,
                    analysis_id,
                    logical_trace_id,
                    revision as i64,
                    diagnostic,
                ],
            )?;
        }
    }
    for group in groups {
        let representative = group.finding_ids.iter().find_map(|finding_id| {
            findings
                .iter()
                .find(|finding| &finding.finding_id == finding_id)
        });
        let subject = representative
            .and_then(|finding| finding.metadata.get("subject"))
            .and_then(Value::as_str);
        let operation = representative
            .and_then(|finding| finding.metadata.get("operation"))
            .and_then(Value::as_str);
        let presentation_json = representative
            .and_then(|finding| FindingPresenter.present(finding))
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        transaction.execute(
            "INSERT INTO active_failure_group_memberships(
                logical_trace_id, group_id, projection_schema_version, project_id,
                revision, service_name, environment, build_id, session_id,
                run_title, run_started_at_unix_nano, analysis_id, failure_signature,
                subject, operation, presentation_json, telemetry_gaps_json,
                telemetry_gap_count, detector_ids_json, finding_ids_json,
                occurrence_count, severity, recovered_count, unrecovered_count,
                unknown_recovery_count, confirmed_count, dismissed_count,
                needs_context_count, unreviewed_count, stale_disposition_count,
                first_seen_at, last_seen_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                ?21, ?22, ?23, ?24, ?25, 0, 0, 0, ?26, 0, ?27, ?28
             )",
            params![
                logical_trace_id,
                group.group_id,
                ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION,
                project_id,
                revision as i64,
                service_name,
                environment,
                build_id,
                session_id,
                run_title,
                run_started_at,
                analysis_id,
                group.failure_signature,
                subject,
                operation,
                presentation_json,
                telemetry_gaps_json,
                telemetry_gaps.len() as i64,
                serde_json::to_string(&group.detector_ids)?,
                serde_json::to_string(&group.finding_ids)?,
                group.occurrence_count as i64,
                finding_severity_name(group.severity),
                group.recovery_counts.get("recovered").copied().unwrap_or(0) as i64,
                group
                    .recovery_counts
                    .get("unrecovered")
                    .copied()
                    .unwrap_or(0) as i64,
                group.recovery_counts.get("unknown").copied().unwrap_or(0) as i64,
                group.occurrence_count as i64,
                group.first_seen_at,
                group.last_seen_at,
            ],
        )?;
        for detector_id in &group.detector_ids {
            transaction.execute(
                "INSERT INTO active_failure_group_detectors(
                    logical_trace_id, group_id, project_id, detector_id
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![logical_trace_id, group.group_id, project_id, detector_id],
            )?;
        }
        refresh_failure_membership_dispositions(transaction, logical_trace_id, &group.group_id)?;
    }
    transaction.execute(
        "INSERT INTO active_failure_projection_state(
            logical_trace_id, revision, analysis_id, projection_schema_version,
            projected_at_unix_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(logical_trace_id) DO UPDATE SET
            revision = excluded.revision,
            analysis_id = excluded.analysis_id,
            projection_schema_version = excluded.projection_schema_version,
            projected_at_unix_ms = excluded.projected_at_unix_ms",
        params![
            logical_trace_id,
            revision as i64,
            analysis_id,
            ACTIVE_FAILURE_PROJECTION_SCHEMA_VERSION,
            now_unix_ms()
        ],
    )?;
    Ok(())
}
