//! Thread, run, attachment, and runtime-state persistence.

use anyhow::{Context, Result};
use rusqlite::params;
use tracing::warn;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::{
    AttachmentSource, ProviderKind, Thread, ThreadAttachment, ThreadMessage, ThreadRun,
    ThreadRunStatus, ThreadRuntimeState, ThreadStatus,
};

use super::optional;

const THREAD_COLUMNS: &str = "\
    id, project_id, github_item_id, task_id, session_id, title, status, provider_kind, \
    provider_profile, worktree_path, branch_name, runtime_profile, workflow_run_id, \
    created_at, updated_at";

const THREAD_MESSAGE_COLUMNS: &str =
    "id, thread_id, run_id, role, content, attachments_json, created_at";

const THREAD_RUN_COLUMNS: &str = "\
    id, thread_id, provider_kind, provider_profile, status, prompt, started_at, completed_at, \
    error_message";

const THREAD_ATTACHMENT_COLUMNS: &str = "\
    id, thread_id, message_id, draft_key, mime_type, file_name, width, height, local_path, \
    source, created_at";

const THREAD_RUNTIME_STATE_COLUMNS: &str =
    "id, thread_id, profile_name, build_status, services_json, last_error, updated_at";

impl Store {
    #[expect(
        clippy::too_many_arguments,
        reason = "thread creation persists the full launch context"
    )]
    pub fn create_thread(
        &self,
        project_id: &str,
        github_item_id: Option<&str>,
        task_id: Option<&str>,
        session_id: Option<&str>,
        title: &str,
        provider_kind: ProviderKind,
        provider_profile: Option<&str>,
        worktree_path: Option<&str>,
        branch_name: Option<&str>,
        runtime_profile: Option<&str>,
        workflow_run_id: Option<&str>,
    ) -> Result<Thread> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO threads (
                id, project_id, github_item_id, task_id, session_id, title, status, provider_kind,
                provider_profile, worktree_path, branch_name, runtime_profile, workflow_run_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                id,
                project_id,
                github_item_id,
                task_id,
                session_id,
                title,
                ThreadStatus::Draft.as_str(),
                provider_kind.as_str(),
                provider_profile,
                worktree_path,
                branch_name,
                runtime_profile,
                workflow_run_id
            ],
        )?;
        self.get_thread(&id)
    }

    pub fn get_thread(&self, id: &str) -> Result<Thread> {
        let sql = format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_thread)
            .with_context(|| format!("failed to fetch thread '{id}'"))
    }

    pub fn find_thread_for_task(&self, task_id: &str) -> Result<Option<Thread>> {
        let sql = format!("SELECT {THREAD_COLUMNS} FROM threads WHERE task_id = ?1");
        optional(
            self.conn
                .query_row(&sql, params![task_id], Self::row_to_thread),
        )
        .with_context(|| format!("failed to fetch thread for task '{task_id}'"))
    }

    pub fn find_thread_for_session(&self, session_id: &str) -> Result<Option<Thread>> {
        let sql = format!(
            "SELECT {THREAD_COLUMNS} FROM threads WHERE session_id = ?1 ORDER BY updated_at DESC LIMIT 1"
        );
        optional(
            self.conn
                .query_row(&sql, params![session_id], Self::row_to_thread),
        )
        .with_context(|| format!("failed to fetch thread for session '{session_id}'"))
    }

    pub fn find_thread_for_github_item(&self, github_item_id: &str) -> Result<Option<Thread>> {
        let sql = format!("SELECT {THREAD_COLUMNS} FROM threads WHERE github_item_id = ?1");
        optional(
            self.conn
                .query_row(&sql, params![github_item_id], Self::row_to_thread),
        )
        .with_context(|| format!("failed to fetch thread for GitHub item '{github_item_id}'"))
    }

    pub fn list_threads(&self) -> Result<Vec<Thread>> {
        let sql = format!(
            "SELECT {THREAD_COLUMNS} FROM threads ORDER BY updated_at DESC, created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map([], Self::row_to_thread)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn list_threads_for_project(&self, project_id: &str) -> Result<Vec<Thread>> {
        let sql = format!(
            "SELECT {THREAD_COLUMNS} FROM threads WHERE project_id = ?1 ORDER BY updated_at DESC, created_at DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![project_id], Self::row_to_thread)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn update_thread_status(&self, id: &str, status: ThreadStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), chrono::Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn update_thread_provider(
        &self,
        id: &str,
        provider_kind: ProviderKind,
        provider_profile: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET provider_kind = ?1, provider_profile = ?2, updated_at = ?3 WHERE id = ?4",
            params![
                provider_kind.as_str(),
                provider_profile,
                chrono::Utc::now().to_rfc3339(),
                id
            ],
        )?;
        Ok(())
    }

    pub fn update_thread_session_and_worktree(
        &self,
        id: &str,
        session_id: Option<&str>,
        worktree_path: Option<&str>,
        branch_name: Option<&str>,
        runtime_profile: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE threads
             SET session_id = ?1, worktree_path = ?2, branch_name = ?3, runtime_profile = ?4, updated_at = ?5
             WHERE id = ?6",
            params![
                session_id,
                worktree_path,
                branch_name,
                runtime_profile,
                chrono::Utc::now().to_rfc3339(),
                id
            ],
        )?;
        Ok(())
    }

    pub fn attach_workflow_run_to_thread(
        &self,
        thread_id: &str,
        workflow_run_id: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE threads SET workflow_run_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![workflow_run_id, chrono::Utc::now().to_rfc3339(), thread_id],
        )?;
        Ok(())
    }

    pub fn create_thread_message(
        &self,
        thread_id: &str,
        run_id: Option<&str>,
        role: &str,
        content: &str,
        attachments: &[String],
    ) -> Result<ThreadMessage> {
        let id = Uuid::new_v4().to_string();
        let attachments_json = serde_json::to_string(attachments)?;
        self.conn.execute(
            "INSERT INTO thread_messages (id, thread_id, run_id, role, content, attachments_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, thread_id, run_id, role, content, attachments_json],
        )?;
        self.get_thread_message(&id)
    }

    pub fn get_thread_message(&self, id: &str) -> Result<ThreadMessage> {
        let sql = format!("SELECT {THREAD_MESSAGE_COLUMNS} FROM thread_messages WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_thread_message)
            .with_context(|| format!("failed to fetch thread message '{id}'"))
    }

    pub fn list_thread_messages(&self, thread_id: &str) -> Result<Vec<ThreadMessage>> {
        let sql = format!(
            "SELECT {THREAD_MESSAGE_COLUMNS} FROM thread_messages WHERE thread_id = ?1 ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![thread_id], Self::row_to_thread_message)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn create_thread_run(
        &self,
        thread_id: &str,
        provider_kind: ProviderKind,
        provider_profile: Option<&str>,
        status: ThreadRunStatus,
        prompt: Option<&str>,
    ) -> Result<ThreadRun> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO thread_runs (id, thread_id, provider_kind, provider_profile, status, prompt)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                thread_id,
                provider_kind.as_str(),
                provider_profile,
                status.as_str(),
                prompt
            ],
        )?;
        self.get_thread_run(&id)
    }

    pub fn get_thread_run(&self, id: &str) -> Result<ThreadRun> {
        let sql = format!("SELECT {THREAD_RUN_COLUMNS} FROM thread_runs WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_thread_run)
            .with_context(|| format!("failed to fetch thread run '{id}'"))
    }

    pub fn list_thread_runs(&self, thread_id: &str) -> Result<Vec<ThreadRun>> {
        let sql = format!(
            "SELECT {THREAD_RUN_COLUMNS} FROM thread_runs WHERE thread_id = ?1 ORDER BY started_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![thread_id], Self::row_to_thread_run)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn update_thread_run_status(
        &self,
        id: &str,
        status: ThreadRunStatus,
        error_message: Option<&str>,
    ) -> Result<()> {
        let completed_at = matches!(status, ThreadRunStatus::Done | ThreadRunStatus::Error)
            .then(|| chrono::Utc::now().to_rfc3339());
        self.conn.execute(
            "UPDATE thread_runs
             SET status = ?1, error_message = ?2, completed_at = COALESCE(?3, completed_at)
             WHERE id = ?4",
            params![status.as_str(), error_message, completed_at, id],
        )?;
        Ok(())
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "attachments need full metadata for clipboard/file parity"
    )]
    pub fn create_thread_attachment(
        &self,
        thread_id: &str,
        message_id: Option<&str>,
        draft_key: Option<&str>,
        mime_type: &str,
        file_name: &str,
        width: Option<i64>,
        height: Option<i64>,
        local_path: &str,
        source: AttachmentSource,
    ) -> Result<ThreadAttachment> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO thread_attachments (
                id, thread_id, message_id, draft_key, mime_type, file_name, width, height,
                local_path, source
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                thread_id,
                message_id,
                draft_key,
                mime_type,
                file_name,
                width,
                height,
                local_path,
                source.as_str()
            ],
        )?;
        self.get_thread_attachment(&id)
    }

    pub fn get_thread_attachment(&self, id: &str) -> Result<ThreadAttachment> {
        let sql =
            format!("SELECT {THREAD_ATTACHMENT_COLUMNS} FROM thread_attachments WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id], Self::row_to_thread_attachment)
            .with_context(|| format!("failed to fetch thread attachment '{id}'"))
    }

    pub fn list_thread_attachments(&self, thread_id: &str) -> Result<Vec<ThreadAttachment>> {
        let sql = format!(
            "SELECT {THREAD_ATTACHMENT_COLUMNS} FROM thread_attachments WHERE thread_id = ?1 ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![thread_id], Self::row_to_thread_attachment)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    /// List draft attachments (not yet linked to any message) for a thread.
    pub fn list_draft_attachments(&self, thread_id: &str) -> Result<Vec<ThreadAttachment>> {
        let sql = format!(
            "SELECT {THREAD_ATTACHMENT_COLUMNS} FROM thread_attachments \
             WHERE thread_id = ?1 AND message_id IS NULL ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let items = stmt
            .query_map(params![thread_id], Self::row_to_thread_attachment)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(items)
    }

    /// Link all draft attachments for a thread to a specific message.
    pub fn link_attachments_to_message(&self, thread_id: &str, message_id: &str) -> Result<usize> {
        let count = self.conn.execute(
            "UPDATE thread_attachments SET message_id = ?1 \
             WHERE thread_id = ?2 AND message_id IS NULL",
            params![message_id, thread_id],
        )?;
        Ok(count)
    }

    pub fn upsert_thread_runtime_state(
        &self,
        thread_id: &str,
        profile_name: Option<&str>,
        build_status: Option<&str>,
        services_json: &str,
        last_error: Option<&str>,
    ) -> Result<ThreadRuntimeState> {
        if self.get_thread_runtime_state(thread_id)?.is_some() {
            self.conn.execute(
                "UPDATE thread_runtime_state
                 SET profile_name = ?1, build_status = ?2, services_json = ?3, last_error = ?4, updated_at = ?5
                 WHERE thread_id = ?6",
                params![
                    profile_name,
                    build_status,
                    services_json,
                    last_error,
                    chrono::Utc::now().to_rfc3339(),
                    thread_id
                ],
            )?;
            return self.get_thread_runtime_state(thread_id)?.with_context(|| {
                format!("thread runtime state disappeared after update for thread '{thread_id}'")
            });
        }

        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO thread_runtime_state (id, thread_id, profile_name, build_status, services_json, last_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, thread_id, profile_name, build_status, services_json, last_error],
        )?;
        self.get_thread_runtime_state(thread_id)?.with_context(|| {
            format!("failed to fetch newly-created runtime state for thread '{thread_id}'")
        })
    }

    pub fn get_thread_runtime_state(&self, thread_id: &str) -> Result<Option<ThreadRuntimeState>> {
        let sql = format!(
            "SELECT {THREAD_RUNTIME_STATE_COLUMNS} FROM thread_runtime_state WHERE thread_id = ?1"
        );
        optional(
            self.conn
                .query_row(&sql, params![thread_id], Self::row_to_thread_runtime_state),
        )
        .with_context(|| format!("failed to fetch runtime state for thread '{thread_id}'"))
    }

    fn row_to_thread(row: &rusqlite::Row<'_>) -> rusqlite::Result<Thread> {
        let status_str: String = row.get(6)?;
        let provider_str: String = row.get(7)?;
        let id: String = row.get(0)?;
        let status = status_str.parse().unwrap_or_else(|_| {
            warn!(thread_id = %id, raw = %status_str, "unknown thread status in DB, defaulting to Draft");
            ThreadStatus::Draft
        });
        let provider_kind = provider_str.parse().unwrap_or_else(|_| {
            warn!(thread_id = %id, raw = %provider_str, "unknown provider kind in DB, defaulting to Unknown");
            ProviderKind::Unknown
        });
        Ok(Thread {
            id,
            project_id: row.get(1)?,
            github_item_id: row.get(2)?,
            task_id: row.get(3)?,
            session_id: row.get(4)?,
            title: row.get(5)?,
            status,
            provider_kind,
            provider_profile: row.get(8)?,
            worktree_path: row.get(9)?,
            branch_name: row.get(10)?,
            runtime_profile: row.get(11)?,
            workflow_run_id: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
        })
    }

    fn row_to_thread_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadMessage> {
        let attachments_json: String = row.get(5)?;
        Ok(ThreadMessage {
            id: row.get(0)?,
            thread_id: row.get(1)?,
            run_id: row.get(2)?,
            role: row.get(3)?,
            content: row.get(4)?,
            attachments: serde_json::from_str(&attachments_json).unwrap_or_default(),
            created_at: row.get(6)?,
        })
    }

    fn row_to_thread_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadRun> {
        let provider_str: String = row.get(2)?;
        let status_str: String = row.get(4)?;
        let id: String = row.get(0)?;
        let provider_kind = provider_str.parse().unwrap_or_else(|_| {
            warn!(thread_run_id = %id, raw = %provider_str, "unknown thread provider in DB, defaulting to Unknown");
            ProviderKind::Unknown
        });
        let status = status_str.parse().unwrap_or_else(|_| {
            warn!(thread_run_id = %id, raw = %status_str, "unknown thread run status in DB, defaulting to Pending");
            ThreadRunStatus::Pending
        });
        Ok(ThreadRun {
            id,
            thread_id: row.get(1)?,
            provider_kind,
            provider_profile: row.get(3)?,
            status,
            prompt: row.get(5)?,
            started_at: row.get(6)?,
            completed_at: row.get(7)?,
            error_message: row.get(8)?,
        })
    }

    fn row_to_thread_attachment(row: &rusqlite::Row<'_>) -> rusqlite::Result<ThreadAttachment> {
        let source_str: String = row.get(9)?;
        let id: String = row.get(0)?;
        let source = source_str.parse().unwrap_or_else(|_| {
            warn!(attachment_id = %id, raw = %source_str, "unknown attachment source in DB, defaulting to File");
            AttachmentSource::File
        });
        Ok(ThreadAttachment {
            id,
            thread_id: row.get(1)?,
            message_id: row.get(2)?,
            draft_key: row.get(3)?,
            mime_type: row.get(4)?,
            file_name: row.get(5)?,
            width: row.get(6)?,
            height: row.get(7)?,
            local_path: row.get(8)?,
            source,
            created_at: row.get(10)?,
        })
    }

    fn row_to_thread_runtime_state(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<ThreadRuntimeState> {
        Ok(ThreadRuntimeState {
            id: row.get(0)?,
            thread_id: row.get(1)?,
            profile_name: row.get(2)?,
            build_status: row.get(3)?,
            services_json: row.get(4)?,
            last_error: row.get(5)?,
            updated_at: row.get(6)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{AttachmentSource, ProviderKind, Store, ThreadRunStatus, ThreadStatus};

    #[test]
    fn thread_round_trip_persists_messages_runtime_and_attachments() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let thread = store
            .create_thread(
                &project.id,
                None,
                None,
                None,
                "Thread title",
                ProviderKind::Claude,
                Some("default"),
                Some("/tmp/proj/thread"),
                Some("task/test"),
                Some("default"),
                None,
            )
            .unwrap();
        store
            .update_thread_status(&thread.id, ThreadStatus::Ready)
            .unwrap();
        let run = store
            .create_thread_run(
                &thread.id,
                ProviderKind::Claude,
                Some("default"),
                ThreadRunStatus::Pending,
                Some("Plan this"),
            )
            .unwrap();
        let attachment = store
            .create_thread_attachment(
                &thread.id,
                None,
                Some("draft-1"),
                "image/png",
                "shot.png",
                Some(10),
                Some(10),
                "/tmp/shot.png",
                AttachmentSource::Clipboard,
            )
            .unwrap();
        let message = store
            .create_thread_message(
                &thread.id,
                Some(&run.id),
                "user",
                "hello",
                std::slice::from_ref(&attachment.id),
            )
            .unwrap();
        let runtime_state = store
            .upsert_thread_runtime_state(
                &thread.id,
                Some("default"),
                Some("built"),
                r#"[{"name":"api","status":"running"}]"#,
                None,
            )
            .unwrap();

        let fetched = store.get_thread(&thread.id).unwrap();
        assert_eq!(fetched.title, "Thread title");
        assert_eq!(fetched.status, ThreadStatus::Ready);
        let messages = store.list_thread_messages(&thread.id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, message.id);
        let runs = store.list_thread_runs(&thread.id).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, run.id);
        let attachments = store.list_thread_attachments(&thread.id).unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].source, AttachmentSource::Clipboard);
        assert_eq!(runtime_state.thread_id, thread.id);
    }

    #[test]
    fn find_thread_for_session_returns_linked_thread() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let session = store
            .create_session(&project.id, "task/test", "/tmp/proj-thread", "tab")
            .unwrap();
        let thread = store
            .create_thread(
                &project.id,
                None,
                None,
                Some(&session.id),
                "Thread title",
                ProviderKind::Claude,
                Some("default"),
                Some("/tmp/proj/thread"),
                Some("task/test"),
                Some("default"),
                None,
            )
            .unwrap();

        let fetched = store
            .find_thread_for_session(&session.id)
            .unwrap()
            .expect("thread should be found");
        assert_eq!(fetched.id, thread.id);
    }
}
