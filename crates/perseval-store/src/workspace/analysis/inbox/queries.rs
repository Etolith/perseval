use super::*;

type GroupKey = (String, String);

impl WorkspaceStore {
    pub(super) fn load_failure_recurrence_window(
        &self,
        scope: &QueryScopeV1,
    ) -> Result<Option<FailureRecurrenceWindow>, StoreError> {
        const MAX_BUCKET_COUNT: usize = 7;
        let criteria = &scope.criteria;
        let control = self.control.lock().expect("control store lock poisoned");
        let bounds = control.query_row(
            "SELECT MIN(start_time_unix_nano), MAX(start_time_unix_nano), COUNT(*)
               FROM logical_traces
              WHERE workspace_id = ?1
                AND analysis_status IN ('ready', 'reanalyzing')
                AND (?2 IS NULL OR project_id = ?2)
                AND (?3 IS NULL OR service_name = ?3)
                AND (?4 IS NULL OR environment = ?4)
                AND (?5 IS NULL OR build_id = ?5)
                AND (?6 IS NULL OR session_id = ?6)
                AND (?7 IS NULL OR start_time_unix_nano >= ?7)
                AND (?8 IS NULL OR start_time_unix_nano <= ?8)",
            params![
                self.workspace_id,
                criteria.project_id,
                criteria.service_name,
                criteria.environment,
                criteria.build_id,
                criteria.session_id,
                criteria.started_after_unix_nano.map(|value| value as i64),
                criteria.started_before_unix_nano.map(|value| value as i64),
            ],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        let (Some(first_run), Some(last_run), eligible_run_total) = bounds else {
            return Ok(None);
        };
        let started_at_unix_nano = criteria
            .started_after_unix_nano
            .unwrap_or(first_run.max(0) as u64);
        let last_inclusive = criteria
            .started_before_unix_nano
            .unwrap_or(last_run.max(0) as u64);
        let ended_at_unix_nano = last_inclusive
            .saturating_add(1)
            .max(started_at_unix_nano.saturating_add(1));
        let duration = ended_at_unix_nano.saturating_sub(started_at_unix_nano);
        let bucket_count = usize::try_from(eligible_run_total.max(1))
            .unwrap_or(MAX_BUCKET_COUNT)
            .min(MAX_BUCKET_COUNT);
        let bucket_width_nano = duration
            .saturating_add(bucket_count as u64 - 1)
            .checked_div(bucket_count as u64)
            .unwrap_or(1)
            .max(1);
        let mut eligible_run_counts = vec![0_u64; bucket_count];
        let mut statement = control.prepare(
            "SELECT CAST((start_time_unix_nano - ?9) / ?11 AS INTEGER), COUNT(*)
               FROM logical_traces
              WHERE workspace_id = ?1
                AND analysis_status IN ('ready', 'reanalyzing')
                AND (?2 IS NULL OR project_id = ?2)
                AND (?3 IS NULL OR service_name = ?3)
                AND (?4 IS NULL OR environment = ?4)
                AND (?5 IS NULL OR build_id = ?5)
                AND (?6 IS NULL OR session_id = ?6)
                AND (?7 IS NULL OR start_time_unix_nano >= ?7)
                AND (?8 IS NULL OR start_time_unix_nano <= ?8)
                AND start_time_unix_nano >= ?9
                AND start_time_unix_nano < ?10
              GROUP BY 1",
        )?;
        let rows = statement.query_map(
            params![
                self.workspace_id,
                criteria.project_id,
                criteria.service_name,
                criteria.environment,
                criteria.build_id,
                criteria.session_id,
                criteria.started_after_unix_nano.map(|value| value as i64),
                criteria.started_before_unix_nano.map(|value| value as i64),
                started_at_unix_nano as i64,
                ended_at_unix_nano as i64,
                bucket_width_nano as i64,
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        for row in rows {
            let (index, count) = row?;
            if let Some(bucket) = eligible_run_counts.get_mut(index.max(0) as usize) {
                *bucket = count.max(0) as u64;
            }
        }
        Ok(Some(FailureRecurrenceWindow {
            started_at_unix_nano,
            ended_at_unix_nano,
            bucket_width_nano,
            eligible_run_counts,
        }))
    }

    pub fn has_active_findings(&self, project_id: Option<&str>) -> Result<bool, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        if let Some(project_id) = project_id {
            return control
                .query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM logical_traces
                        WHERE workspace_id = ?1 AND project_id = ?2
                          AND analysis_status = 'ready' AND finding_count > 0
                    )",
                    params![self.workspace_id, project_id],
                    |row| row.get(0),
                )
                .map_err(StoreError::from);
        }
        control
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM logical_traces
                    WHERE workspace_id = ?1
                      AND analysis_status = 'ready' AND finding_count > 0
                )",
                params![self.workspace_id],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub fn list_failure_groups(
        &self,
        filters: &FailureFiltersV1,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<FailureGroupSummary>, StoreError> {
        Ok(self.list_failure_group_page(filters, offset, limit)?.rows)
    }

    pub fn list_failure_group_page(
        &self,
        filters: &FailureFiltersV1,
        offset: u64,
        limit: u32,
    ) -> Result<FailureGroupPageV1, StoreError> {
        filters.scope.validate().map_err(StoreError::Invalid)?;
        let (rows, total) = self.load_materialized_failure_groups(filters, None, offset, limit)?;
        let summaries = self.hydrate_materialized_failure_groups(filters, rows, false)?;
        Ok(FailureGroupPageV1 {
            offset,
            total,
            rows: summaries,
        })
    }

    pub(super) fn load_materialized_failure_groups(
        &self,
        filters: &FailureFiltersV1,
        group_id: Option<&str>,
        offset: u64,
        limit: u32,
    ) -> Result<(Vec<MaterializedFailureGroupRow>, u64), StoreError> {
        let scope = &filters.scope.criteria;
        let severity = filters.severity.map(finding_severity_name);
        let recovery = filters.recovery.map(finding_recovery_name);
        let search = normalized_failure_search(filters.search.as_deref());
        let control = self.control.lock().expect("control store lock poisoned");
        let count = control.query_row(
            "WITH filtered AS (
                SELECT m.*
                  FROM active_failure_group_memberships m
                  JOIN logical_traces t ON t.logical_trace_id = m.logical_trace_id
                 WHERE t.workspace_id = ?1
                   AND (?2 IS NULL OR m.project_id = ?2)
                   AND (?3 IS NULL OR m.service_name = ?3)
                   AND (?4 IS NULL OR m.environment = ?4)
                   AND (?5 IS NULL OR m.build_id = ?5)
                   AND (?6 IS NULL OR m.session_id = ?6)
                   AND (?7 IS NULL OR m.run_started_at_unix_nano >= ?7)
                   AND (?8 IS NULL OR m.run_started_at_unix_nano <= ?8)
                   AND (?9 IS NULL OR m.severity = ?9)
                   AND (?10 IS NULL OR CASE ?10
                       WHEN 'recovered' THEN m.recovered_count
                       WHEN 'unrecovered' THEN m.unrecovered_count
                       ELSE m.unknown_recovery_count END > 0)
                   AND (?11 IS NULL OR EXISTS (
                       SELECT 1 FROM active_failure_group_detectors d
                        WHERE d.logical_trace_id = m.logical_trace_id
                          AND d.group_id = m.group_id AND d.detector_id = ?11))
                   AND (?12 IS NULL OR LOWER(
                       m.failure_signature || ' ' || COALESCE(m.subject, '') || ' ' ||
                       COALESCE(m.operation, '') || ' ' || m.detector_ids_json
                   ) LIKE ?12)
                   AND (?13 IS NULL OR m.group_id = ?13)
            ), grouped AS (
                SELECT project_id, group_id,
                       SUM(occurrence_count) AS occurrence_count,
                       SUM(dismissed_count) AS dismissed_count
                  FROM filtered GROUP BY project_id, group_id
            )
            SELECT COUNT(*) FROM grouped
             WHERE ?14 <> 0 OR dismissed_count < occurrence_count",
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
                filters.include_fully_dismissed,
            ],
            |row| row.get::<_, i64>(0),
        )?;
        let mut statement = control.prepare(
            "WITH filtered AS (
                SELECT m.*, t.analysis_status
                  FROM active_failure_group_memberships m
                  JOIN logical_traces t ON t.logical_trace_id = m.logical_trace_id
                 WHERE t.workspace_id = ?1
                   AND (?2 IS NULL OR m.project_id = ?2)
                   AND (?3 IS NULL OR m.service_name = ?3)
                   AND (?4 IS NULL OR m.environment = ?4)
                   AND (?5 IS NULL OR m.build_id = ?5)
                   AND (?6 IS NULL OR m.session_id = ?6)
                   AND (?7 IS NULL OR m.run_started_at_unix_nano >= ?7)
                   AND (?8 IS NULL OR m.run_started_at_unix_nano <= ?8)
                   AND (?9 IS NULL OR m.severity = ?9)
                   AND (?10 IS NULL OR CASE ?10
                       WHEN 'recovered' THEN m.recovered_count
                       WHEN 'unrecovered' THEN m.unrecovered_count
                       ELSE m.unknown_recovery_count END > 0)
                   AND (?11 IS NULL OR EXISTS (
                       SELECT 1 FROM active_failure_group_detectors d
                        WHERE d.logical_trace_id = m.logical_trace_id
                          AND d.group_id = m.group_id AND d.detector_id = ?11))
                   AND (?12 IS NULL OR LOWER(
                       m.failure_signature || ' ' || COALESCE(m.subject, '') || ' ' ||
                       COALESCE(m.operation, '') || ' ' || m.detector_ids_json
                   ) LIKE ?12)
                   AND (?13 IS NULL OR m.group_id = ?13)
            ), grouped AS (
                SELECT project_id, group_id,
                       MAX(failure_signature) AS failure_signature,
                       MAX(subject) AS subject, MAX(operation) AS operation,
                       MAX(presentation_json) AS presentation_json,
                       MAX(CASE severity WHEN 'critical' THEN 4 WHEN 'high' THEN 3
                           WHEN 'medium' THEN 2 WHEN 'low' THEN 1 ELSE 0 END)
                           AS severity_rank,
                       SUM(occurrence_count) AS occurrence_count,
                       SUM(recovered_count) AS recovered_count,
                       SUM(unrecovered_count) AS unrecovered_count,
                       SUM(unknown_recovery_count) AS unknown_recovery_count,
                       COUNT(DISTINCT logical_trace_id) AS affected_run_count,
                       COUNT(DISTINCT build_id) AS affected_build_count,
                       COUNT(DISTINCT environment) AS affected_environment_count,
                       SUM(confirmed_count) AS confirmed_count,
                       SUM(dismissed_count) AS dismissed_count,
                       SUM(needs_context_count) AS needs_context_count,
                       SUM(unreviewed_count) AS unreviewed_count,
                       SUM(stale_disposition_count) AS stale_disposition_count,
                       MIN(first_seen_at) AS first_seen_at,
                       MAX(last_seen_at) AS last_seen_at,
                       SUM(telemetry_gap_count) AS telemetry_gap_count,
                       MAX(CASE analysis_status WHEN 'reanalyzing' THEN 1 ELSE 0 END)
                           AS reanalyzing
                  FROM filtered GROUP BY project_id, group_id
            )
            SELECT g.*,
                   COALESCE((SELECT json_group_array(detector_id) FROM (
                       SELECT DISTINCT detector_id
                         FROM active_failure_group_detectors d
                        WHERE d.project_id = g.project_id AND d.group_id = g.group_id
                        ORDER BY detector_id)), '[]')
              FROM grouped g
             WHERE ?14 <> 0 OR g.dismissed_count < g.occurrence_count
             ORDER BY g.severity_rank DESC, g.unrecovered_count DESC,
                      g.occurrence_count DESC, g.last_seen_at DESC,
                      g.project_id, g.group_id
             LIMIT ?15 OFFSET ?16",
        )?;
        let rows = statement.query_map(
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
                filters.include_fully_dismissed,
                limit,
                offset,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, i64>(17)?,
                    row.get::<_, i64>(18)?,
                    row.get::<_, String>(19)?,
                    row.get::<_, String>(20)?,
                    row.get::<_, i64>(21)?,
                    row.get::<_, i64>(22)?,
                    row.get::<_, String>(23)?,
                ))
            },
        )?;
        let rows = rows
            .map(|row| {
                let (
                    project_id,
                    group_id,
                    failure_signature,
                    subject,
                    operation,
                    presentation,
                    severity_rank,
                    occurrence_count,
                    recovered_count,
                    unrecovered_count,
                    unknown_recovery_count,
                    affected_run_count,
                    affected_build_count,
                    affected_environment_count,
                    confirmed_count,
                    dismissed_count,
                    needs_context_count,
                    unreviewed_count,
                    stale_disposition_count,
                    first_seen_at,
                    last_seen_at,
                    telemetry_gap_count,
                    reanalyzing,
                    detector_ids,
                ) = row?;
                Ok(MaterializedFailureGroupRow {
                    project_id,
                    group_id,
                    failure_signature,
                    detector_ids: serde_json::from_str(&detector_ids)?,
                    subject,
                    operation,
                    presentation: presentation
                        .map(|value| serde_json::from_str(&value))
                        .transpose()?,
                    severity: finding_severity_from_rank(severity_rank),
                    occurrence_count: nonnegative(occurrence_count),
                    recovered_count: nonnegative(recovered_count),
                    unrecovered_count: nonnegative(unrecovered_count),
                    unknown_recovery_count: nonnegative(unknown_recovery_count),
                    affected_run_count: nonnegative(affected_run_count),
                    affected_build_count: nonnegative(affected_build_count),
                    affected_environment_count: nonnegative(affected_environment_count),
                    confirmed_count: nonnegative(confirmed_count),
                    dismissed_count: nonnegative(dismissed_count),
                    needs_context_count: nonnegative(needs_context_count),
                    unreviewed_count: nonnegative(unreviewed_count),
                    stale_disposition_count: nonnegative(stale_disposition_count),
                    first_seen_at,
                    last_seen_at,
                    telemetry_gap_count: nonnegative(telemetry_gap_count),
                    reanalyzing: reanalyzing != 0,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok((rows, nonnegative(count)))
    }

    pub(super) fn hydrate_materialized_failure_groups(
        &self,
        filters: &FailureFiltersV1,
        rows: Vec<MaterializedFailureGroupRow>,
        include_feature_similarity: bool,
    ) -> Result<Vec<FailureGroupSummary>, StoreError> {
        let recurrence = self.load_materialized_recurrence(filters, &rows)?;
        let cohorts = if include_feature_similarity {
            self.load_materialized_feature_similarity_cohorts(filters, &rows)?
        } else {
            BTreeMap::new()
        };
        Ok(rows
            .into_iter()
            .map(|row| {
                let key = (row.project_id.clone(), row.group_id.clone());
                let recurrence = recurrence.get(&key).cloned();
                let occurrence_trend = recurrence
                    .as_ref()
                    .map(|series| {
                        series
                            .buckets
                            .iter()
                            .map(|bucket| bucket.finding_count)
                            .collect()
                    })
                    .unwrap_or_default();
                FailureGroupSummary {
                    scope: failure_group_scope(&filters.scope, &row.project_id),
                    project_id: row.project_id,
                    group_id: row.group_id,
                    failure_signature: row.failure_signature,
                    detector_ids: row.detector_ids,
                    subject: row.subject,
                    operation: row.operation,
                    presentation: row.presentation,
                    severity: row.severity,
                    occurrence_count: row.occurrence_count,
                    recovered_count: row.recovered_count,
                    unrecovered_count: row.unrecovered_count,
                    unknown_recovery_count: row.unknown_recovery_count,
                    affected_run_count: row.affected_run_count,
                    affected_build_count: row.affected_build_count,
                    affected_environment_count: row.affected_environment_count,
                    confirmed_count: row.confirmed_count,
                    dismissed_count: row.dismissed_count,
                    needs_context_count: row.needs_context_count,
                    unreviewed_count: row.unreviewed_count,
                    stale_disposition_count: row.stale_disposition_count,
                    first_seen_at: row.first_seen_at,
                    last_seen_at: row.last_seen_at,
                    occurrence_trend,
                    recurrence,
                    telemetry_gap_count: row.telemetry_gap_count,
                    reanalyzing: row.reanalyzing,
                    feature_similarity_cohorts: cohorts.get(&key).cloned().unwrap_or_default(),
                }
            })
            .collect())
    }

    fn load_materialized_recurrence(
        &self,
        filters: &FailureFiltersV1,
        groups: &[MaterializedFailureGroupRow],
    ) -> Result<BTreeMap<GroupKey, FailureRecurrenceSeriesV1>, StoreError> {
        let mut result = BTreeMap::new();
        let project_ids = groups
            .iter()
            .map(|group| group.project_id.as_str())
            .collect::<BTreeSet<_>>();
        for project_id in project_ids {
            let project_scope = failure_group_scope(&filters.scope, project_id);
            let Some(window) = self.load_failure_recurrence_window(&project_scope)? else {
                continue;
            };
            let group_ids = groups
                .iter()
                .filter(|group| group.project_id == project_id)
                .map(|group| group.group_id.clone())
                .collect::<Vec<_>>();
            let encoded_groups = serde_json::to_string(&group_ids)?;
            let scope = &filters.scope.criteria;
            let severity = filters.severity.map(finding_severity_name);
            let recovery = filters.recovery.map(finding_recovery_name);
            let search = normalized_failure_search(filters.search.as_deref());
            let control = self.control.lock().expect("control store lock poisoned");
            let mut statement = control.prepare(
                "SELECT m.group_id,
                        CAST((m.run_started_at_unix_nano - ?2) / ?4 AS INTEGER),
                        SUM(m.occurrence_count), COUNT(DISTINCT m.logical_trace_id)
                   FROM active_failure_group_memberships m
                   JOIN json_each(?1) requested ON requested.value = m.group_id
                  WHERE m.project_id = ?5
                    AND m.run_started_at_unix_nano >= ?2
                    AND m.run_started_at_unix_nano < ?3
                    AND (?6 IS NULL OR m.service_name = ?6)
                    AND (?7 IS NULL OR m.environment = ?7)
                    AND (?8 IS NULL OR m.build_id = ?8)
                    AND (?9 IS NULL OR m.session_id = ?9)
                    AND (?10 IS NULL OR m.run_started_at_unix_nano >= ?10)
                    AND (?11 IS NULL OR m.run_started_at_unix_nano <= ?11)
                    AND (?12 IS NULL OR m.severity = ?12)
                    AND (?13 IS NULL OR CASE ?13 WHEN 'recovered' THEN m.recovered_count
                         WHEN 'unrecovered' THEN m.unrecovered_count
                         ELSE m.unknown_recovery_count END > 0)
                    AND (?14 IS NULL OR EXISTS (
                        SELECT 1 FROM active_failure_group_detectors d
                         WHERE d.logical_trace_id = m.logical_trace_id
                           AND d.group_id = m.group_id AND d.detector_id = ?14))
                    AND (?15 IS NULL OR LOWER(
                        m.failure_signature || ' ' || COALESCE(m.subject, '') || ' ' ||
                        COALESCE(m.operation, '') || ' ' || m.detector_ids_json
                    ) LIKE ?15)
                  GROUP BY m.group_id, 2",
            )?;
            let rows = statement.query_map(
                params![
                    encoded_groups,
                    window.started_at_unix_nano as i64,
                    window.ended_at_unix_nano as i64,
                    window.bucket_width_nano as i64,
                    project_id,
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
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )?;
            let mut counts = BTreeMap::<String, Vec<(u64, u64)>>::new();
            for group_id in &group_ids {
                counts.insert(
                    group_id.clone(),
                    vec![(0, 0); window.eligible_run_counts.len()],
                );
            }
            for row in rows {
                let (group_id, bucket, findings, affected_runs) = row?;
                if let Some(value) = counts
                    .get_mut(&group_id)
                    .and_then(|values| values.get_mut(bucket.max(0) as usize))
                {
                    *value = (nonnegative(findings), nonnegative(affected_runs));
                }
            }
            for (group_id, counts) in counts {
                let buckets = counts
                    .into_iter()
                    .enumerate()
                    .map(|(index, (finding_count, affected_run_count))| {
                        let started_at_unix_nano = window
                            .started_at_unix_nano
                            .saturating_add(window.bucket_width_nano.saturating_mul(index as u64));
                        let ended_at_unix_nano = started_at_unix_nano
                            .saturating_add(window.bucket_width_nano)
                            .min(window.ended_at_unix_nano);
                        let eligible_run_count = window.eligible_run_counts[index];
                        FailureRecurrenceBucketV1 {
                            started_at_unix_nano,
                            ended_at_unix_nano,
                            eligible_run_count,
                            affected_run_count,
                            finding_count,
                            recurrence_rate_basis_points: recurrence_rate_basis_points(
                                affected_run_count,
                                eligible_run_count,
                            ),
                        }
                    })
                    .collect();
                result.insert(
                    (project_id.to_string(), group_id),
                    FailureRecurrenceSeriesV1 {
                        started_at_unix_nano: window.started_at_unix_nano,
                        ended_at_unix_nano: window.ended_at_unix_nano,
                        bucket_width_nano: window.bucket_width_nano,
                        buckets,
                    },
                );
            }
        }
        Ok(result)
    }

    fn load_materialized_feature_similarity_cohorts(
        &self,
        filters: &FailureFiltersV1,
        groups: &[MaterializedFailureGroupRow],
    ) -> Result<BTreeMap<GroupKey, Vec<FeatureSimilarityCohortSummary>>, StoreError> {
        let mut result = BTreeMap::<GroupKey, Vec<FeatureSimilarityCohortSummary>>::new();
        let project_ids = groups
            .iter()
            .map(|group| group.project_id.as_str())
            .collect::<BTreeSet<_>>();
        for project_id in project_ids {
            let group_ids = groups
                .iter()
                .filter(|group| group.project_id == project_id)
                .map(|group| group.group_id.clone())
                .collect::<Vec<_>>();
            let encoded_groups = serde_json::to_string(&group_ids)?;
            let scope = &filters.scope.criteria;
            let severity = filters.severity.map(finding_severity_name);
            let recovery = filters.recovery.map(finding_recovery_name);
            let search = normalized_failure_search(filters.search.as_deref());
            let control = self.control.lock().expect("control store lock poisoned");
            let mut statement = control.prepare(
                "SELECT f.group_id, a.model_id, a.cluster_id, COUNT(*), AVG(a.confidence),
                        SUM(a.novelty), GROUP_CONCAT(DISTINCT a.method),
                        json_extract(model.model_json, '$.source.embedding_provider'),
                        json_extract(model.model_json, '$.source.embedding_model')
                   FROM active_failure_findings f
                   JOIN json_each(?1) requested ON requested.value = f.group_id
                   JOIN semantic_cluster_assignments a ON a.finding_id = f.finding_id
                   JOIN semantic_cluster_models model ON model.model_id = a.model_id
                  WHERE f.project_id = ?2 AND model.project_id = ?2 AND model.active = 1
                    AND model.model_id = (
                        SELECT latest.model_id FROM semantic_cluster_models latest
                         WHERE latest.project_id = ?2 AND latest.active = 1
                         ORDER BY latest.created_at_unix_ms DESC, latest.model_id DESC LIMIT 1)
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
                        f.failure_signature || ' ' || COALESCE(f.subject, '') || ' ' ||
                        COALESCE(f.operation, '') || ' ' || f.detector_id
                    ) LIKE ?12)
                  GROUP BY f.group_id, a.model_id, a.cluster_id
                  ORDER BY f.group_id, COUNT(*) DESC, a.cluster_id",
            )?;
            let rows = statement.query_map(
                params![
                    encoded_groups,
                    project_id,
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
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        FeatureSimilarityCohortSummary {
                            model_id: row.get(1)?,
                            cluster_id: row.get(2)?,
                            member_count: nonnegative(row.get(3)?),
                            mean_confidence: row.get::<_, f64>(4)? as f32,
                            novelty_count: nonnegative(row.get(5)?),
                            method: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                            embedding_provider: row.get(7)?,
                            embedding_model: row.get(8)?,
                        },
                    ))
                },
            )?;
            for row in rows {
                let (group_id, cohort) = row?;
                result
                    .entry((project_id.to_string(), group_id))
                    .or_default()
                    .push(cohort);
            }
        }
        Ok(result)
    }
}

fn nonnegative(value: i64) -> u64 {
    value.max(0) as u64
}

fn finding_severity_from_rank(rank: i64) -> FindingSeverity {
    match rank {
        4.. => FindingSeverity::Critical,
        3 => FindingSeverity::High,
        2 => FindingSeverity::Medium,
        1 => FindingSeverity::Low,
        _ => FindingSeverity::Info,
    }
}
