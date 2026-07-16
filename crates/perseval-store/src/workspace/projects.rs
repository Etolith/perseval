use super::*;

impl WorkspaceStore {
    pub fn list_projects(&self) -> Result<Vec<ProjectV1>, StoreError> {
        let control = self.control.lock().expect("control store lock poisoned");
        let mut statement = control.prepare(
            "SELECT project_id, display_name, artifact_namespace,
                    created_at_unix_ms, updated_at_unix_ms
             FROM projects
             WHERE workspace_id = ?1
             ORDER BY display_name COLLATE NOCASE, project_id",
        )?;
        Ok(statement
            .query_map(params![self.workspace_id], |row| {
                Ok(ProjectV1 {
                    schema_version: PROJECT_SCHEMA_VERSION.into(),
                    workspace_id: self.workspace_id.clone(),
                    project_id: row.get(0)?,
                    display_name: row.get(1)?,
                    artifact_namespace: row.get(2)?,
                    created_at_unix_ms: row.get(3)?,
                    updated_at_unix_ms: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub fn create_project(&self, request: &CreateProjectV1) -> Result<ProjectV1, StoreError> {
        validate_project(request)?;
        let now = now_unix_ms();
        let control = self.control.lock().expect("control store lock poisoned");
        control.execute(
            "INSERT INTO projects(
                workspace_id, project_id, display_name, artifact_namespace,
                created_at_unix_ms, updated_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![
                self.workspace_id,
                request.project_id.trim(),
                request.display_name.trim(),
                request.artifact_namespace.trim(),
                now,
            ],
        )?;
        Ok(ProjectV1 {
            schema_version: PROJECT_SCHEMA_VERSION.into(),
            workspace_id: self.workspace_id.clone(),
            project_id: request.project_id.trim().into(),
            display_name: request.display_name.trim().into(),
            artifact_namespace: request.artifact_namespace.trim().into(),
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
        })
    }
}

fn validate_project(request: &CreateProjectV1) -> Result<(), StoreError> {
    let id = request.project_id.trim();
    if id.is_empty() || id.len() > 80 {
        return Err(StoreError::Invalid(
            "project_id must contain between 1 and 80 characters".into(),
        ));
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(StoreError::Invalid(
            "project_id may contain only letters, numbers, '-', '_', and '.'".into(),
        ));
    }
    if matches!(
        id.to_ascii_lowercase().as_str(),
        "unassigned" | "all-projects"
    ) {
        return Err(StoreError::Invalid(format!(
            "project_id '{id}' is reserved by the workspace scope model"
        )));
    }
    let display_name = request.display_name.trim();
    if display_name.is_empty() || display_name.chars().count() > 120 {
        return Err(StoreError::Invalid(
            "display_name must contain between 1 and 120 characters".into(),
        ));
    }
    let namespace = request.artifact_namespace.trim();
    if namespace.is_empty() || namespace.chars().count() > 160 {
        return Err(StoreError::Invalid(
            "artifact_namespace must contain between 1 and 160 characters".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn project_creation_is_persisted_and_workspace_scoped() {
        let directory = tempdir().unwrap();
        let layout = WorkspaceStoreLayout::new(directory.path());
        let store = WorkspaceStore::open(&layout, "workspace-a").unwrap();
        let created = store
            .create_project(&CreateProjectV1 {
                project_id: "checkout-agent".into(),
                display_name: "Checkout Agent".into(),
                artifact_namespace: "checkout".into(),
            })
            .unwrap();

        assert_eq!(created.project_id, "checkout-agent");
        drop(store);

        let reopened = WorkspaceStore::open(&layout, "workspace-a").unwrap();
        assert_eq!(reopened.list_projects().unwrap(), vec![created]);
        let other = WorkspaceStore::open(&layout, "workspace-b").unwrap();
        assert!(other.list_projects().unwrap().is_empty());
    }

    #[test]
    fn project_ids_reject_ambiguous_or_path_like_values() {
        let directory = tempdir().unwrap();
        let store =
            WorkspaceStore::open(&WorkspaceStoreLayout::new(directory.path()), "workspace-a")
                .unwrap();
        let error = store
            .create_project(&CreateProjectV1 {
                project_id: "../checkout".into(),
                display_name: "Checkout".into(),
                artifact_namespace: "checkout".into(),
            })
            .unwrap_err();
        assert!(matches!(error, StoreError::Invalid(_)));
    }
}
