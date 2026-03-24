//! Workflow definition and run persistence.

use anyhow::{Context, Result};
use rusqlite::params;
use tracing::warn;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::{
    ProviderKind, WorkflowArtifact, WorkflowDef, WorkflowRun, WorkflowRunStatus, WorkflowStageRun,
    WorkflowStageStatus,
};

use super::optional;

const WORKFLOW_DEF_COLUMNS: &str = "\
    id, name, scope, source_path, description, definition_yaml, built_in, created_at, updated_at";

const WORKFLOW_RUN_COLUMNS: &str = "\
    id, workflow_def_id, thread_id, github_item_id, status, current_stage, started_at, completed_at";

const WORKFLOW_STAGE_RUN_COLUMNS: &str = "\
    id, workflow_run_id, stage_name, status, provider_kind, provider_profile, runtime_profile, \
    prompt, gate_state, output_summary, started_at, completed_at";

const WORKFLOW_ARTIFACT_COLUMNS: &str = "\
    id, workflow_run_id, stage_run_id, artifact_name, artifact_type, local_path, content_text, created_at";

impl Store {
    pub fn upsert_workflow_def(
        &self,
        name: &str,
        scope: &str,
        source_path: Option<&str>,
        description: Option<&str>,
        definition_yaml: &str,
        built_in: bool,
    ) -> Result<WorkflowDef> {
        if let Some(existing) = self.get_workflow_def_by_name(name)? {
            self.conn.execute(
                "UPDATE workflow_defs
                 SET scope = ?1, source_path = ?2, description = ?3, definition_yaml = ?4, built_in = ?5, updated_at = ?6
                 WHERE id = ?7",
                params![
                    scope,
                    source_path,
                    description,
                    definition_yaml,
                    built_in,
                    chrono::Utc::now().to_rfc3339(),
                    existing.id
                ],
            )?;
            return self.get_workflow_def(&existing.id);
        }

        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO workflow_defs (id, name, scope, source_path, description, definition_yaml, built_in)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, name, scope, source_path, description, definition_yaml, built_in],
        )?;
        self.get_workflow_def(&id)
    }

    pub fn get_workflow_def(&self, id: &str) -> Result<WorkflowDef> {
        let sql = format!("SELECT {WORKFLOW_DEF_COLUMNS} FROM workflow_defs WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_workflow_def)
            .with_context(|| format!("failed to fetch workflow definition '{id}'"))
    }

    pub fn get_workflow_def_by_name(&self, name: &str) -> Result<Option<WorkflowDef>> {
        let sql = format!("SELECT {WORKFLOW_DEF_COLUMNS} FROM workflow_defs WHERE name = ?1");
        optional(
            self.conn
                .query_row(&sql, params![name], Self::row_to_workflow_def),
        )
        .with_context(|| format!("failed to fetch workflow definition '{name}'"))
    }

    pub fn list_workflow_defs(&self) -> Result<Vec<WorkflowDef>> {
        let sql = format!("SELECT {WORKFLOW_DEF_COLUMNS} FROM workflow_defs ORDER BY name");
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map([], Self::row_to_workflow_def)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn create_workflow_run(
        &self,
        workflow_def_id: &str,
        thread_id: Option<&str>,
        github_item_id: Option<&str>,
        status: WorkflowRunStatus,
        current_stage: Option<&str>,
    ) -> Result<WorkflowRun> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO workflow_runs (id, workflow_def_id, thread_id, github_item_id, status, current_stage)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                workflow_def_id,
                thread_id,
                github_item_id,
                status.as_str(),
                current_stage
            ],
        )?;
        self.get_workflow_run(&id)
    }

    pub fn get_workflow_run(&self, id: &str) -> Result<WorkflowRun> {
        let sql = format!("SELECT {WORKFLOW_RUN_COLUMNS} FROM workflow_runs WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_workflow_run)
            .with_context(|| format!("failed to fetch workflow run '{id}'"))
    }

    pub fn list_workflow_runs(&self) -> Result<Vec<WorkflowRun>> {
        let sql =
            format!("SELECT {WORKFLOW_RUN_COLUMNS} FROM workflow_runs ORDER BY started_at DESC");
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map([], Self::row_to_workflow_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn update_workflow_run_status(
        &self,
        id: &str,
        status: WorkflowRunStatus,
        current_stage: Option<&str>,
    ) -> Result<()> {
        let completed_at = matches!(
            status,
            WorkflowRunStatus::Completed | WorkflowRunStatus::Failed | WorkflowRunStatus::Cancelled
        )
        .then(|| chrono::Utc::now().to_rfc3339());
        self.conn.execute(
            "UPDATE workflow_runs
             SET status = ?1, current_stage = ?2, completed_at = COALESCE(?3, completed_at)
             WHERE id = ?4",
            params![status.as_str(), current_stage, completed_at, id],
        )?;
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "workflow stage runs capture full stage execution context"
    )]
    pub fn create_workflow_stage_run(
        &self,
        workflow_run_id: &str,
        stage_name: &str,
        status: WorkflowStageStatus,
        provider_kind: Option<ProviderKind>,
        provider_profile: Option<&str>,
        runtime_profile: Option<&str>,
        prompt: Option<&str>,
        gate_state: Option<&str>,
        output_summary: Option<&str>,
    ) -> Result<WorkflowStageRun> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO workflow_stage_runs (
                id, workflow_run_id, stage_name, status, provider_kind, provider_profile,
                runtime_profile, prompt, gate_state, output_summary
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                workflow_run_id,
                stage_name,
                status.as_str(),
                provider_kind.map(|kind| kind.as_str().to_string()),
                provider_profile,
                runtime_profile,
                prompt,
                gate_state,
                output_summary
            ],
        )?;
        self.get_workflow_stage_run(&id)
    }

    pub fn get_workflow_stage_run(&self, id: &str) -> Result<WorkflowStageRun> {
        let sql =
            format!("SELECT {WORKFLOW_STAGE_RUN_COLUMNS} FROM workflow_stage_runs WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_workflow_stage_run)
            .with_context(|| format!("failed to fetch workflow stage run '{id}'"))
    }

    pub fn list_workflow_stage_runs(&self, workflow_run_id: &str) -> Result<Vec<WorkflowStageRun>> {
        let sql = format!(
            "SELECT {WORKFLOW_STAGE_RUN_COLUMNS} FROM workflow_stage_runs WHERE workflow_run_id = ?1 ORDER BY started_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![workflow_run_id], Self::row_to_workflow_stage_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn update_workflow_stage_run_status(
        &self,
        id: &str,
        status: WorkflowStageStatus,
        gate_state: Option<&str>,
        output_summary: Option<&str>,
    ) -> Result<()> {
        let completed_at = matches!(
            status,
            WorkflowStageStatus::Completed
                | WorkflowStageStatus::Failed
                | WorkflowStageStatus::Skipped
        )
        .then(|| chrono::Utc::now().to_rfc3339());
        self.conn.execute(
            "UPDATE workflow_stage_runs
             SET status = ?1, gate_state = ?2, output_summary = ?3, completed_at = COALESCE(?4, completed_at)
             WHERE id = ?5",
            params![status.as_str(), gate_state, output_summary, completed_at, id],
        )?;
        Ok(())
    }

    pub fn create_workflow_artifact(
        &self,
        workflow_run_id: &str,
        stage_run_id: Option<&str>,
        artifact_name: &str,
        artifact_type: &str,
        local_path: Option<&str>,
        content_text: Option<&str>,
    ) -> Result<WorkflowArtifact> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO workflow_artifacts (
                id, workflow_run_id, stage_run_id, artifact_name, artifact_type, local_path, content_text
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                workflow_run_id,
                stage_run_id,
                artifact_name,
                artifact_type,
                local_path,
                content_text
            ],
        )?;
        self.get_workflow_artifact(&id)
    }

    pub fn get_workflow_artifact(&self, id: &str) -> Result<WorkflowArtifact> {
        let sql =
            format!("SELECT {WORKFLOW_ARTIFACT_COLUMNS} FROM workflow_artifacts WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_workflow_artifact)
            .with_context(|| format!("failed to fetch workflow artifact '{id}'"))
    }

    pub fn list_workflow_artifacts(&self, workflow_run_id: &str) -> Result<Vec<WorkflowArtifact>> {
        let sql = format!(
            "SELECT {WORKFLOW_ARTIFACT_COLUMNS} FROM workflow_artifacts WHERE workflow_run_id = ?1 ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![workflow_run_id], Self::row_to_workflow_artifact)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    fn row_to_workflow_def(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowDef> {
        Ok(WorkflowDef {
            id: row.get(0)?,
            name: row.get(1)?,
            scope: row.get(2)?,
            source_path: row.get(3)?,
            description: row.get(4)?,
            definition_yaml: row.get(5)?,
            built_in: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    fn row_to_workflow_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRun> {
        let status_str: String = row.get(4)?;
        let id: String = row.get(0)?;
        let status = status_str.parse().unwrap_or_else(|_| {
            warn!(workflow_run_id = %id, raw = %status_str, "unknown workflow run status in DB, defaulting to Pending");
            WorkflowRunStatus::Pending
        });
        Ok(WorkflowRun {
            id,
            workflow_def_id: row.get(1)?,
            thread_id: row.get(2)?,
            github_item_id: row.get(3)?,
            status,
            current_stage: row.get(5)?,
            started_at: row.get(6)?,
            completed_at: row.get(7)?,
        })
    }

    fn row_to_workflow_stage_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowStageRun> {
        let status_str: String = row.get(3)?;
        let provider_str: Option<String> = row.get(4)?;
        let id: String = row.get(0)?;
        let status = status_str.parse().unwrap_or_else(|_| {
            warn!(workflow_stage_run_id = %id, raw = %status_str, "unknown workflow stage status in DB, defaulting to Pending");
            WorkflowStageStatus::Pending
        });
        Ok(WorkflowStageRun {
            id,
            workflow_run_id: row.get(1)?,
            stage_name: row.get(2)?,
            status,
            provider_kind: provider_str.and_then(|raw| raw.parse::<ProviderKind>().ok()),
            provider_profile: row.get(5)?,
            runtime_profile: row.get(6)?,
            prompt: row.get(7)?,
            gate_state: row.get(8)?,
            output_summary: row.get(9)?,
            started_at: row.get(10)?,
            completed_at: row.get(11)?,
        })
    }

    fn row_to_workflow_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowArtifact> {
        Ok(WorkflowArtifact {
            id: row.get(0)?,
            workflow_run_id: row.get(1)?,
            stage_run_id: row.get(2)?,
            artifact_name: row.get(3)?,
            artifact_type: row.get(4)?,
            local_path: row.get(5)?,
            content_text: row.get(6)?,
            created_at: row.get(7)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{ProviderKind, Store, WorkflowRunStatus, WorkflowStageStatus};

    #[test]
    fn workflow_definition_and_run_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let def = store
            .upsert_workflow_def(
                "plan_first_tdd",
                "builtin",
                None,
                Some("test workflow"),
                "name: plan_first_tdd",
                true,
            )
            .unwrap();
        let run = store
            .create_workflow_run(
                &def.id,
                None,
                None,
                WorkflowRunStatus::Running,
                Some("plan"),
            )
            .unwrap();
        let stage = store
            .create_workflow_stage_run(
                &run.id,
                "plan",
                WorkflowStageStatus::Running,
                Some(ProviderKind::Claude),
                Some("default"),
                Some("default"),
                Some("Prompt"),
                None,
                None,
            )
            .unwrap();
        let artifact = store
            .create_workflow_artifact(
                &run.id,
                Some(&stage.id),
                "plan.md",
                "markdown",
                Some("/tmp/plan.md"),
                None,
            )
            .unwrap();

        assert_eq!(store.list_workflow_defs().unwrap().len(), 1);
        assert_eq!(store.list_workflow_stage_runs(&run.id).unwrap().len(), 1);
        assert_eq!(
            store.list_workflow_artifacts(&run.id).unwrap()[0].id,
            artifact.id
        );
    }
}
