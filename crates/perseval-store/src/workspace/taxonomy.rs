use super::context::{ensure_project_exists, validate_project_scope};
use super::*;

use crate::model::{ReviewAuthorityV1, TaxonomyChangeDraftRecordV1, TaxonomyGovernanceSummaryV1};
use traces_to_evals::{AgentTaxonomyReleaseV1, TaxonomyLineageOperationV1, TaxonomyNodeV1};

impl WorkspaceStore {
    pub fn taxonomy_governance_summary(
        &self,
        project_id: &str,
    ) -> Result<TaxonomyGovernanceSummaryV1, StoreError> {
        validate_project_scope(project_id)?;
        let control = self.control.lock().expect("control store lock poisoned");
        ensure_project_exists(&control, &self.workspace_id, project_id)?;
        let drafts_in_review = control.query_row(
            "SELECT COUNT(*) FROM taxonomy_change_drafts WHERE project_id = ?1 AND status = 'review'",
            params![project_id],
            |row| row.get::<_, i64>(0),
        )? as u64;
        let latest_draft = control
            .query_row(
                "SELECT draft_id, base_release_id, draft_json, source_manifest_json,
                        created_by, created_at_unix_ms
                 FROM taxonomy_change_drafts
                 WHERE project_id = ?1 AND status = 'review'
                 ORDER BY created_at_unix_ms DESC, rowid DESC LIMIT 1",
                params![project_id],
                |row| {
                    let proposal_json: String = row.get(2)?;
                    let source_manifest_json: String = row.get(3)?;
                    Ok(TaxonomyChangeDraftRecordV1 {
                        draft_id: row.get(0)?,
                        project_id: project_id.into(),
                        base_release_id: row.get(1)?,
                        proposal: serde_json::from_str(&proposal_json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                proposal_json.len(),
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        source_manifest: serde_json::from_str(&source_manifest_json).map_err(
                            |error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    source_manifest_json.len(),
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            },
                        )?,
                        created_by: row.get(4)?,
                        created_at_unix_ms: row.get(5)?,
                    })
                },
            )
            .optional()?;
        let (active_release_count, active_node_count, latest_release_id) = control.query_row(
            "SELECT
                (SELECT COUNT(*) FROM taxonomy_releases WHERE project_id = ?1),
                (SELECT COUNT(*) FROM taxonomy_nodes n
                 WHERE n.taxonomy_release_id = (
                    SELECT taxonomy_release_id FROM taxonomy_releases
                    WHERE project_id = ?1
                    ORDER BY activated_at_unix_ms DESC, rowid DESC LIMIT 1
                 )),
                (SELECT taxonomy_release_id FROM taxonomy_releases
                 WHERE project_id = ?1
                 ORDER BY activated_at_unix_ms DESC, rowid DESC LIMIT 1)",
            params![project_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )?;
        let active_nodes = if let Some(release_id) = latest_release_id.as_deref() {
            let release_json: String = control.query_row(
                "SELECT release_json FROM taxonomy_releases
                 WHERE taxonomy_release_id = ?1 AND project_id = ?2",
                params![release_id, project_id],
                |row| row.get(0),
            )?;
            serde_json::from_str::<AgentTaxonomyReleaseV1>(&release_json)?.nodes
        } else {
            Vec::<TaxonomyNodeV1>::new()
        };
        Ok(TaxonomyGovernanceSummaryV1 {
            project_id: project_id.into(),
            drafts_in_review,
            active_release_count,
            active_node_count,
            latest_draft_id: latest_draft.as_ref().map(|draft| draft.draft_id.clone()),
            latest_release_id,
            latest_draft,
            active_nodes,
        })
    }

    pub fn active_taxonomy_release(
        &self,
        project_id: &str,
    ) -> Result<Option<(String, AgentTaxonomyReleaseV1)>, StoreError> {
        validate_project_scope(project_id)?;
        let control = self.control.lock().expect("control store lock poisoned");
        ensure_project_exists(&control, &self.workspace_id, project_id)?;
        control
            .query_row(
                "SELECT taxonomy_release_id, release_json FROM taxonomy_releases
                 WHERE project_id = ?1
                 ORDER BY activated_at_unix_ms DESC, rowid DESC LIMIT 1",
                params![project_id],
                |row| {
                    let release_id: String = row.get(0)?;
                    let release_json: String = row.get(1)?;
                    let release = serde_json::from_str(&release_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            release_json.len(),
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok((release_id, release))
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn approve_taxonomy_change_draft(
        &self,
        draft_id: &str,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        if authority != ReviewAuthorityV1::Human {
            return Err(StoreError::Invalid(
                "only a human reviewer can approve a taxonomy change draft".into(),
            ));
        }
        let release = {
            let control = self.control.lock().expect("control store lock poisoned");
            let encoded = control
                .query_row(
                    "SELECT draft_json FROM taxonomy_change_drafts
                     WHERE draft_id = ?1 AND status = 'review'",
                    params![draft_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    StoreError::Invalid("taxonomy draft is missing or already closed".into())
                })?;
            serde_json::from_str::<AgentTaxonomyReleaseV1>(&encoded)?
        };
        self.activate_taxonomy_release(draft_id, &release, activated_by, authority)
    }

    pub fn create_taxonomy_change_draft(
        &self,
        project_id: &str,
        base_release_id: Option<&str>,
        proposal: &Value,
        source_manifest: &Value,
        created_by: &str,
    ) -> Result<String, StoreError> {
        validate_project_scope(project_id)?;
        if created_by.trim().is_empty() {
            return Err(StoreError::Invalid("created_by must not be empty".into()));
        }
        let now = now_unix_ms();
        let identity = (
            "perseval.taxonomy-change-draft.v1",
            project_id,
            base_release_id,
            proposal,
            source_manifest,
            now,
        );
        let draft_id = taxonomy_identity(&identity)?;
        let control = self.control.lock().expect("control store lock poisoned");
        ensure_project_exists(&control, &self.workspace_id, project_id)?;
        if let Some(base_release_id) = base_release_id {
            let exists = control.query_row(
                "SELECT EXISTS(SELECT 1 FROM taxonomy_releases
                  WHERE taxonomy_release_id = ?1 AND project_id = ?2)",
                params![base_release_id, project_id],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(StoreError::Invalid(
                    "taxonomy draft base release is missing or cross-project".into(),
                ));
            }
        }
        control.execute(
            "INSERT INTO taxonomy_change_drafts(
                draft_id, project_id, base_release_id, status, draft_json,
                source_manifest_json, created_by, created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, 'review', ?4, ?5, ?6, ?7, ?7)",
            params![
                draft_id,
                project_id,
                base_release_id,
                serde_json::to_string(proposal)?,
                serde_json::to_string(source_manifest)?,
                created_by,
                now,
            ],
        )?;
        Ok(draft_id)
    }

    pub fn activate_taxonomy_release(
        &self,
        draft_id: &str,
        release: &AgentTaxonomyReleaseV1,
        activated_by: &str,
        authority: ReviewAuthorityV1,
    ) -> Result<String, StoreError> {
        if authority != ReviewAuthorityV1::Human {
            return Err(StoreError::Invalid(
                "only a human reviewer can activate a taxonomy release".into(),
            ));
        }
        release
            .validate()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let release_id = release
            .release_id()
            .map_err(|error| StoreError::Invalid(error.to_string()))?;
        let now = now_unix_ms();
        let mut control = self.control.lock().expect("control store lock poisoned");
        let transaction = control.transaction()?;
        let (project_id, base_release_id): (String, Option<String>) = transaction
            .query_row(
                "SELECT project_id, base_release_id FROM taxonomy_change_drafts
                 WHERE draft_id = ?1 AND status = 'review'",
                params![draft_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::Invalid("taxonomy draft is missing or already closed".into())
            })?;
        if release.previous_release_id != base_release_id {
            return Err(StoreError::Invalid(
                "taxonomy release previous identity does not match its reviewed draft".into(),
            ));
        }
        if let Some(previous_release_id) = &base_release_id {
            let prior_json: String = transaction.query_row(
                "SELECT release_json FROM taxonomy_releases
                 WHERE taxonomy_release_id = ?1 AND project_id = ?2",
                params![previous_release_id, project_id],
                |row| row.get(0),
            )?;
            let prior: AgentTaxonomyReleaseV1 = serde_json::from_str(&prior_json)?;
            release
                .validate_transition(&prior)
                .map_err(|error| StoreError::Invalid(error.to_string()))?;
        }
        transaction.execute(
            "INSERT INTO taxonomy_releases(
                taxonomy_release_id, project_id, source_draft_id, release_json,
                activated_by, activated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                release_id,
                project_id,
                draft_id,
                serde_json::to_string(release)?,
                activated_by,
                now,
            ],
        )?;
        for node in &release.nodes {
            transaction.execute(
                "INSERT INTO taxonomy_nodes(
                    taxonomy_release_id, node_id, node_kind, node_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    release_id,
                    node.node_id,
                    format!("{:?}", node.dimension).to_ascii_lowercase(),
                    serde_json::to_string(node)?,
                ],
            )?;
        }
        for relation in &release.relations {
            let relation_id = taxonomy_identity(&(
                "perseval.taxonomy-relation.v1",
                &relation.source_node_id,
                relation.kind,
                &relation.target_node_id,
            ))?;
            transaction.execute(
                "INSERT INTO taxonomy_relations(
                    taxonomy_release_id, relation_id, source_node_id, target_node_id,
                    relation_kind, relation_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    release_id,
                    relation_id,
                    relation.source_node_id,
                    relation.target_node_id,
                    format!("{:?}", relation.kind).to_ascii_lowercase(),
                    serde_json::to_string(relation)?,
                ],
            )?;
        }
        for (index, lineage) in release.lineage.iter().enumerate() {
            let lineage_id =
                taxonomy_identity(&("perseval.taxonomy-lineage.v1", &release_id, index, lineage))?;
            transaction.execute(
                "INSERT INTO taxonomy_lineage(
                    taxonomy_release_id, lineage_id, operation_kind, lineage_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    release_id,
                    lineage_id,
                    lineage_kind(lineage),
                    serde_json::to_string(lineage)?,
                ],
            )?;
        }
        transaction.execute(
            "UPDATE taxonomy_change_drafts SET status = 'activated', updated_at_unix_ms = ?2
             WHERE draft_id = ?1",
            params![draft_id, now],
        )?;
        transaction.commit()?;
        Ok(release_id)
    }
}

fn taxonomy_identity<T: serde::Serialize>(value: &T) -> Result<String, StoreError> {
    let material = serde_json::to_vec(value)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(material))))
}

fn lineage_kind(operation: &TaxonomyLineageOperationV1) -> &'static str {
    match operation {
        TaxonomyLineageOperationV1::Create { .. } => "create",
        TaxonomyLineageOperationV1::MatchExisting { .. } => "match_existing",
        TaxonomyLineageOperationV1::Merge { .. } => "merge",
        TaxonomyLineageOperationV1::Split { .. } => "split",
        TaxonomyLineageOperationV1::Reparent { .. } => "reparent",
        TaxonomyLineageOperationV1::Rename { .. } => "rename",
        TaxonomyLineageOperationV1::Retire { .. } => "retire",
    }
}
