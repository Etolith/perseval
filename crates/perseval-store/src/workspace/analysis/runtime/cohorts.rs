use super::*;

impl WorkspaceStore {
    pub fn active_feature_similarity_model(&self) -> Result<Option<ClusterModel>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let encoded = control
            .query_row(
                "SELECT model_json FROM semantic_cluster_models WHERE active = 1
                 ORDER BY created_at_unix_ms DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(StoreError::from))
            .transpose()
    }

    pub fn active_feature_similarity_model_for_project(
        &self,
        project_id: &str,
    ) -> Result<Option<ClusterModel>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let encoded = control
            .query_row(
                "SELECT model_json FROM semantic_cluster_models
                 WHERE active = 1 AND project_id = ?1
                 ORDER BY created_at_unix_ms DESC LIMIT 1",
                params![project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(StoreError::from))
            .transpose()
    }

    pub fn active_feature_similarity_model_for_scope(
        &self,
        project_id: &str,
        analysis_definition_id: &str,
        scope_id: &str,
    ) -> Result<Option<ClusterModel>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let encoded = control
            .query_row(
                "SELECT model_json FROM semantic_cluster_models
                 WHERE active = 1 AND project_id = ?1
                   AND analysis_definition_id = ?2 AND scope_id = ?3
                 ORDER BY created_at_unix_ms DESC LIMIT 1",
                params![project_id, analysis_definition_id, scope_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(StoreError::from))
            .transpose()
    }

    pub fn active_finding_projects(&self) -> Result<Vec<String>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT DISTINCT traces.project_id FROM active_failure_findings findings
             JOIN logical_traces traces USING(logical_trace_id)
             WHERE traces.workspace_id = ?1 AND traces.project_id != ?2
             ORDER BY traces.project_id",
        )?;
        statement
            .query_map(params![self.workspace_id, UNASSIGNED_PROJECT_ID], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn project_for_trace(&self, logical_trace_id: &str) -> Result<Option<String>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        control
            .query_row(
                "SELECT project_id FROM logical_traces
                 WHERE workspace_id = ?1 AND logical_trace_id = ?2",
                params![self.workspace_id, logical_trace_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::from)
    }

    /// Returns true only when the project's currently known burst has crossed the immutable
    /// analysis boundary. This prevents a quiet debounce from fitting an intermediate model while
    /// sibling traces from the same burst are still live or waiting for analysis.
    pub fn project_cohort_input_settled(&self, project_id: &str) -> Result<bool, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let unsettled = control.query_row(
            "SELECT COUNT(*) FROM logical_traces
              WHERE workspace_id = ?1 AND project_id = ?2
                AND (lifecycle != 'finalized'
                     OR analysis_status IN ('not_ready', 'pending', 'reanalyzing', 'analyzing'))",
            params![self.workspace_id, project_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(unsettled == 0)
    }

    pub fn active_findings_for_project_bounded(
        &self,
        project_id: &str,
        maximum_cases: usize,
    ) -> Result<Vec<BehaviorFinding>, StoreError> {
        if maximum_cases == 0 {
            return Ok(Vec::new());
        }
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT findings.finding_json
               FROM active_failure_findings findings
               JOIN logical_traces traces USING(logical_trace_id)
              WHERE traces.workspace_id = ?1 AND traces.project_id = ?2
              ORDER BY findings.finding_id
              LIMIT ?3",
        )?;
        statement
            .query_map(
                params![self.workspace_id, project_id, maximum_cases as i64],
                |row| row.get::<_, String>(0),
            )?
            .map(|encoded| Ok(serde_json::from_str(&encoded?)?))
            .collect()
    }

    pub fn active_findings_for_trace(
        &self,
        logical_trace_id: &str,
    ) -> Result<Vec<BehaviorFinding>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT finding_json FROM active_failure_findings
              WHERE logical_trace_id = ?1 ORDER BY finding_id",
        )?;
        statement
            .query_map(params![logical_trace_id], |row| row.get::<_, String>(0))?
            .map(|encoded| Ok(serde_json::from_str(&encoded?)?))
            .collect()
    }

    pub fn commit_feature_similarity_model(
        &self,
        model: &ClusterModel,
    ) -> Result<bool, StoreError> {
        let project_id = model
            .metadata
            .get("perseval_project_id")
            .and_then(Value::as_str)
            .unwrap_or(UNASSIGNED_PROJECT_ID);
        let analysis_definition_id = model
            .metadata
            .get("perseval_analysis_definition_id")
            .and_then(Value::as_str)
            .unwrap_or("legacy");
        let scope_id = model
            .metadata
            .get("perseval_scope_id")
            .and_then(Value::as_str)
            .unwrap_or("all-time-all-builds");
        self.commit_feature_similarity_model_scoped(
            model,
            project_id,
            analysis_definition_id,
            scope_id,
            3,
        )
    }

    pub fn commit_feature_similarity_model_scoped(
        &self,
        model: &ClusterModel,
        project_id: &str,
        analysis_definition_id: &str,
        scope_id: &str,
        history_limit: usize,
    ) -> Result<bool, StoreError> {
        model
            .validate()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        if project_id.trim().is_empty()
            || analysis_definition_id.trim().is_empty()
            || scope_id.trim().is_empty()
            || history_limit == 0
        {
            return Err(StoreError::Invalid(
                "feature-similarity cohort commits require project, analysis definition, scope, and non-zero history".into(),
            ));
        }
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let existing_scope = transaction
            .query_row(
                "SELECT project_id, analysis_definition_id, scope_id
               FROM semantic_cluster_models WHERE model_id = ?1",
                params![model.model_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((existing_project, existing_definition, existing_scope)) = existing_scope {
            if existing_project != project_id
                || existing_definition != analysis_definition_id
                || existing_scope != scope_id
            {
                return Err(StoreError::Invalid(format!(
                    "feature-similarity model id {} already belongs to another project, analysis definition, or scope",
                    model.model_id
                )));
            }
            transaction.execute(
                "UPDATE semantic_cluster_models
                 SET active = CASE WHEN model_id = ?1 THEN 1 ELSE 0 END
                 WHERE project_id = ?2 AND scope_id = ?3",
                params![model.model_id, project_id, scope_id],
            )?;
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "UPDATE semantic_cluster_models SET active = 0
             WHERE project_id = ?1 AND scope_id = ?2",
            params![project_id, scope_id],
        )?;
        transaction.execute(
            "INSERT INTO semantic_cluster_models(
                model_id, model_json, active, created_at_unix_ms,
                project_id, analysis_definition_id, scope_id
             ) VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6)",
            params![
                model.model_id,
                serde_json::to_string(model)?,
                now_unix_ms(),
                project_id,
                analysis_definition_id,
                scope_id,
            ],
        )?;
        let mut insert = transaction.prepare(
            "INSERT INTO semantic_cluster_assignments(
                model_id, finding_id, cluster_id, confidence, distance, novelty, method
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for assignment in &model.assignments {
            insert.execute(params![
                model.model_id,
                assignment.case_id,
                assignment.cluster_id,
                assignment.confidence,
                assignment.distance,
                assignment.novelty,
                assignment.method,
            ])?;
        }
        drop(insert);
        let stale_models = {
            let mut statement = transaction.prepare(
                "SELECT model_id FROM semantic_cluster_models
                 WHERE project_id = ?1 AND scope_id = ?2
                 ORDER BY active DESC, created_at_unix_ms DESC, model_id DESC
                 LIMIT -1 OFFSET ?3",
            )?;
            statement
                .query_map(params![project_id, scope_id, history_limit as i64], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for stale_model_id in stale_models {
            transaction.execute(
                "DELETE FROM semantic_cluster_assignments WHERE model_id = ?1",
                params![stale_model_id],
            )?;
            transaction.execute(
                "DELETE FROM semantic_cluster_models WHERE model_id = ?1",
                params![stale_model_id],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn append_active_feature_similarity_assignments(
        &self,
        project_id: &str,
        analysis_definition_id: &str,
        scope_id: &str,
        assignments: &[ClusterAssignment],
    ) -> Result<u64, StoreError> {
        if assignments.is_empty() {
            return Ok(0);
        }
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let row = transaction
            .query_row(
                "SELECT model_id, model_json FROM semantic_cluster_models
                 WHERE project_id = ?1 AND analysis_definition_id = ?2
                   AND scope_id = ?3 AND active = 1
                 ORDER BY created_at_unix_ms DESC LIMIT 1",
                params![project_id, analysis_definition_id, scope_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((model_id, encoded)) = row else {
            return Ok(0);
        };
        let mut model: ClusterModel = serde_json::from_str(&encoded)?;
        let mut known = model
            .assignments
            .iter()
            .map(|assignment| assignment.case_id.clone())
            .collect::<HashSet<_>>();
        let mut inserted = 0_u64;
        let mut novel = 0_u64;
        for assignment in assignments {
            if !known.insert(assignment.case_id.clone()) {
                continue;
            }
            transaction.execute(
                "INSERT OR IGNORE INTO semantic_cluster_assignments(
                    model_id, finding_id, cluster_id, confidence, distance, novelty, method
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    model_id,
                    assignment.case_id,
                    assignment.cluster_id,
                    assignment.confidence,
                    assignment.distance,
                    assignment.novelty,
                    assignment.method,
                ],
            )?;
            if let Some(cluster) = model
                .clusters
                .iter_mut()
                .find(|cluster| cluster.id == assignment.cluster_id)
            {
                cluster.size = cluster.size.saturating_add(1);
                cluster.quality.size = cluster.quality.size.saturating_add(1);
            }
            model.assignments.push(assignment.clone());
            inserted = inserted.saturating_add(1);
            if assignment.novelty {
                novel = novel.saturating_add(1);
            }
        }
        if inserted > 0 {
            model.quality.assigned_case_count = model
                .quality
                .assigned_case_count
                .saturating_add(inserted as usize);
            let previous_incremental = model
                .metadata
                .get("perseval_incremental_assignment_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            model.metadata.insert(
                "perseval_incremental_assignment_count".into(),
                Value::from(previous_incremental.saturating_add(inserted)),
            );
            let previous_novel = model
                .metadata
                .get("perseval_incremental_novelty_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            model.metadata.insert(
                "perseval_incremental_novelty_count".into(),
                Value::from(previous_novel.saturating_add(novel)),
            );
            model
                .validate()
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
            transaction.execute(
                "UPDATE semantic_cluster_models SET model_json = ?1 WHERE model_id = ?2",
                params![serde_json::to_string(&model)?, model_id],
            )?;
        }
        transaction.commit()?;
        Ok(inserted)
    }
}
