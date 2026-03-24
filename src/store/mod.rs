//! `SQLite` persistence layer.
//!
//! Manages the database connection, versioned schema migrations, and
//! re-exports models and query methods used by the rest of the crate.

mod models;
mod queries;

pub use models::{
    AttachmentSource, CiStatus, ClaudeProgressItem, ClaudeStatus, ExternalSession,
    GitHubCommentCache, GitHubIssueCache, GitHubItem, GitHubItemKind, GitHubPrCache,
    GitHubProjectV2Cache, GitHubRepoCache, GitHubReviewCache, GitHubReviewCommentCache,
    KnowledgeCard, KnowledgeDraft, KnowledgeLink, Project, ProviderKind, PushMode, RateLimitState,
    RuntimeServiceKind, RuntimeServiceStatus, Session, Subtask, Task, TaskMode, TaskStatus,
    TaskStatusCounts, Thread, ThreadAttachment, ThreadMessage, ThreadRun, ThreadRunStatus,
    ThreadRuntimeState, ThreadStatus, WorkflowArtifact, WorkflowDef, WorkflowRun,
    WorkflowRunStatus, WorkflowStageRun, WorkflowStageStatus,
};
pub use queries::ProjectStats;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::config;

struct Migration {
    version: i64,
    sql: &'static str,
}

static MIGRATIONS: &[Migration] = &[
    Migration {
    version: 1,
    sql: "
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                repo_path TEXT NOT NULL UNIQUE,
                default_branch TEXT NOT NULL DEFAULT 'main',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id),
                branch_name TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                tab_label TEXT NOT NULL,
                claude_status TEXT NOT NULL DEFAULT 'idle',
                status_message TEXT NOT NULL DEFAULT '',
                last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
                files_changed INTEGER NOT NULL DEFAULT 0,
                lines_added INTEGER NOT NULL DEFAULT 0,
                lines_removed INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                closed_at TEXT,
                claude_progress TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id),
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'pending',
                mode TEXT NOT NULL DEFAULT 'supervised',
                session_id TEXT REFERENCES sessions(id),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                sort_order INTEGER NOT NULL DEFAULT 0,
                pr_url TEXT
            );

            CREATE TABLE IF NOT EXISTS subtasks (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'pending',
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT
            );

            CREATE TABLE rate_limit_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                is_rate_limited INTEGER NOT NULL DEFAULT 0,
                limit_type TEXT,
                rate_limited_at TEXT,
                reset_at TEXT,
                usage_5h_pct REAL NOT NULL DEFAULT 0.0,
                usage_7d_pct REAL NOT NULL DEFAULT 0.0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            INSERT INTO rate_limit_state (id, is_rate_limited, updated_at)
            VALUES (1, 0, datetime('now'));

            CREATE INDEX IF NOT EXISTS idx_tasks_project_id ON tasks(project_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_session_id ON tasks(session_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_project_closed ON sessions(project_id, closed_at);
            CREATE INDEX IF NOT EXISTS idx_subtasks_task_id ON subtasks(task_id);
        ",
    },
    Migration {
        version: 2,
        sql: "
            CREATE TABLE external_sessions (
                id TEXT PRIMARY KEY,
                project_path TEXT NOT NULL,
                project_name TEXT NOT NULL,
                model TEXT,
                git_branch TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                started_at TEXT,
                ended_at TEXT,
                last_scanned_at TEXT NOT NULL,
                jsonl_path TEXT NOT NULL
            );
            CREATE INDEX idx_external_sessions_project_path ON external_sessions(project_path);
            CREATE INDEX idx_external_sessions_ended_at ON external_sessions(ended_at);
        ",
    },
    Migration {
        version: 3,
        sql: "
            ALTER TABLE tasks ADD COLUMN branch TEXT;
            ALTER TABLE tasks ADD COLUMN push_mode TEXT NOT NULL DEFAULT 'pr';
        ",
    },
    Migration {
        version: 4,
        sql: "
            ALTER TABLE tasks ADD COLUMN ci_status TEXT;
        ",
    },
    Migration {
        version: 5,
        sql: "
            ALTER TABLE tasks ADD COLUMN review_loop INTEGER NOT NULL DEFAULT 0;
        ",
    },
    Migration {
        version: 6,
        sql: "
            ALTER TABLE tasks ADD COLUMN base TEXT;
            UPDATE tasks SET base = branch, branch = NULL;
        ",
    },
    Migration {
        version: 7,
        sql: "
            ALTER TABLE sessions ADD COLUMN claude_session_id TEXT;
        ",
    },
    Migration {
        version: 8,
        sql: "
            ALTER TABLE projects ADD COLUMN is_git_linked INTEGER NOT NULL DEFAULT 1;
        ",
    },
    Migration {
        version: 9,
        sql: "
            CREATE TABLE IF NOT EXISTS github_repos (
                id TEXT PRIMARY KEY,
                project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
                owner TEXT NOT NULL,
                name TEXT NOT NULL,
                full_name TEXT NOT NULL UNIQUE,
                repo_url TEXT,
                default_branch TEXT,
                synced_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS github_projects_v2 (
                id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL REFERENCES github_repos(id) ON DELETE CASCADE,
                project_number INTEGER NOT NULL,
                title TEXT NOT NULL,
                url TEXT,
                synced_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS github_items (
                id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL REFERENCES github_repos(id) ON DELETE CASCADE,
                project_v2_id TEXT REFERENCES github_projects_v2(id) ON DELETE SET NULL,
                node_id TEXT,
                number INTEGER NOT NULL,
                kind TEXT NOT NULL,
                title TEXT NOT NULL,
                state TEXT NOT NULL,
                url TEXT NOT NULL,
                body_text TEXT,
                assignee_logins TEXT NOT NULL DEFAULT '[]',
                label_names TEXT NOT NULL DEFAULT '[]',
                project_field_values TEXT NOT NULL DEFAULT '{}',
                base_ref TEXT,
                head_ref TEXT,
                github_updated_at TEXT,
                synced_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS github_issue_cache (
                item_id TEXT PRIMARY KEY REFERENCES github_items(id) ON DELETE CASCADE,
                body TEXT,
                body_text TEXT,
                body_html TEXT,
                author_login TEXT,
                milestone_title TEXT,
                json_payload TEXT,
                cached_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS github_pr_cache (
                item_id TEXT PRIMARY KEY REFERENCES github_items(id) ON DELETE CASCADE,
                body TEXT,
                body_text TEXT,
                body_html TEXT,
                base_ref TEXT,
                head_ref TEXT,
                merge_state_status TEXT,
                review_decision TEXT,
                is_draft INTEGER NOT NULL DEFAULT 0,
                json_payload TEXT,
                cached_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS github_comment_cache (
                id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL REFERENCES github_items(id) ON DELETE CASCADE,
                github_comment_id TEXT NOT NULL UNIQUE,
                author_login TEXT,
                body TEXT,
                body_text TEXT,
                body_html TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT,
                json_payload TEXT
            );

            CREATE TABLE IF NOT EXISTS github_review_cache (
                id TEXT PRIMARY KEY,
                item_id TEXT NOT NULL REFERENCES github_items(id) ON DELETE CASCADE,
                github_review_id TEXT NOT NULL UNIQUE,
                state TEXT NOT NULL,
                commit_id TEXT,
                author_login TEXT,
                body TEXT,
                body_text TEXT,
                body_html TEXT,
                submitted_at TEXT,
                json_payload TEXT
            );

            CREATE TABLE IF NOT EXISTS github_review_comment_cache (
                id TEXT PRIMARY KEY,
                review_id TEXT REFERENCES github_review_cache(id) ON DELETE SET NULL,
                item_id TEXT NOT NULL REFERENCES github_items(id) ON DELETE CASCADE,
                github_comment_id TEXT NOT NULL UNIQUE,
                author_login TEXT,
                path TEXT,
                line INTEGER,
                side TEXT,
                start_line INTEGER,
                diff_hunk TEXT,
                in_reply_to_id TEXT,
                body TEXT,
                body_text TEXT,
                body_html TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT,
                json_payload TEXT
            );

            CREATE TABLE IF NOT EXISTS workflow_defs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                scope TEXT NOT NULL,
                source_path TEXT,
                description TEXT,
                definition_yaml TEXT NOT NULL,
                built_in INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS workflow_runs (
                id TEXT PRIMARY KEY,
                workflow_def_id TEXT NOT NULL REFERENCES workflow_defs(id) ON DELETE CASCADE,
                thread_id TEXT,
                github_item_id TEXT REFERENCES github_items(id) ON DELETE SET NULL,
                status TEXT NOT NULL,
                current_stage TEXT,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                completed_at TEXT
            );

            CREATE TABLE IF NOT EXISTS workflow_stage_runs (
                id TEXT PRIMARY KEY,
                workflow_run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
                stage_name TEXT NOT NULL,
                status TEXT NOT NULL,
                provider_kind TEXT,
                provider_profile TEXT,
                runtime_profile TEXT,
                prompt TEXT,
                gate_state TEXT,
                output_summary TEXT,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                completed_at TEXT
            );

            CREATE TABLE IF NOT EXISTS workflow_artifacts (
                id TEXT PRIMARY KEY,
                workflow_run_id TEXT NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
                stage_run_id TEXT REFERENCES workflow_stage_runs(id) ON DELETE SET NULL,
                artifact_name TEXT NOT NULL,
                artifact_type TEXT NOT NULL,
                local_path TEXT,
                content_text TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS threads (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                github_item_id TEXT REFERENCES github_items(id) ON DELETE SET NULL,
                task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
                session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                title TEXT NOT NULL,
                status TEXT NOT NULL,
                provider_kind TEXT NOT NULL,
                provider_profile TEXT,
                worktree_path TEXT,
                branch_name TEXT,
                runtime_profile TEXT,
                workflow_run_id TEXT REFERENCES workflow_runs(id) ON DELETE SET NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS thread_runs (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                provider_kind TEXT NOT NULL,
                provider_profile TEXT,
                status TEXT NOT NULL,
                prompt TEXT,
                started_at TEXT NOT NULL DEFAULT (datetime('now')),
                completed_at TEXT,
                error_message TEXT
            );

            CREATE TABLE IF NOT EXISTS thread_messages (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                run_id TEXT REFERENCES thread_runs(id) ON DELETE SET NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                attachments_json TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS thread_attachments (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                message_id TEXT REFERENCES thread_messages(id) ON DELETE SET NULL,
                draft_key TEXT,
                mime_type TEXT NOT NULL,
                file_name TEXT NOT NULL,
                width INTEGER,
                height INTEGER,
                local_path TEXT NOT NULL,
                source TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS thread_runtime_state (
                id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL UNIQUE REFERENCES threads(id) ON DELETE CASCADE,
                profile_name TEXT,
                build_status TEXT,
                services_json TEXT NOT NULL DEFAULT '[]',
                last_error TEXT,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS knowledge_cards (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                github_item_id TEXT REFERENCES github_items(id) ON DELETE SET NULL,
                thread_id TEXT REFERENCES threads(id) ON DELETE SET NULL,
                run_id TEXT REFERENCES thread_runs(id) ON DELETE SET NULL,
                repo_owner TEXT,
                repo_name TEXT,
                title TEXT NOT NULL,
                summary TEXT NOT NULL,
                content_md TEXT NOT NULL,
                confidence REAL,
                tags_json TEXT NOT NULL DEFAULT '[]',
                source_commit TEXT,
                source_branch TEXT,
                extractor TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                accepted_at TEXT
            );

            CREATE TABLE IF NOT EXISTS knowledge_drafts (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                github_item_id TEXT REFERENCES github_items(id) ON DELETE SET NULL,
                thread_id TEXT REFERENCES threads(id) ON DELETE SET NULL,
                run_id TEXT REFERENCES thread_runs(id) ON DELETE SET NULL,
                repo_owner TEXT,
                repo_name TEXT,
                title TEXT NOT NULL,
                summary TEXT NOT NULL,
                content_md TEXT NOT NULL,
                confidence REAL,
                tags_json TEXT NOT NULL DEFAULT '[]',
                source_commit TEXT,
                source_branch TEXT,
                extractor TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                promoted_card_id TEXT REFERENCES knowledge_cards(id) ON DELETE SET NULL
            );

            CREATE TABLE IF NOT EXISTS knowledge_links (
                id TEXT PRIMARY KEY,
                source_type TEXT NOT NULL,
                source_id TEXT NOT NULL,
                target_type TEXT NOT NULL,
                target_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_github_repos_project_id ON github_repos(project_id);
            CREATE INDEX IF NOT EXISTS idx_github_items_repo_id ON github_items(repo_id);
            CREATE INDEX IF NOT EXISTS idx_github_items_kind_number ON github_items(kind, number);
            CREATE INDEX IF NOT EXISTS idx_github_comments_item_id ON github_comment_cache(item_id);
            CREATE INDEX IF NOT EXISTS idx_github_reviews_item_id ON github_review_cache(item_id);
            CREATE INDEX IF NOT EXISTS idx_github_review_comments_item_id ON github_review_comment_cache(item_id);
            CREATE INDEX IF NOT EXISTS idx_threads_project_id ON threads(project_id);
            CREATE INDEX IF NOT EXISTS idx_threads_task_id ON threads(task_id);
            CREATE INDEX IF NOT EXISTS idx_thread_runs_thread_id ON thread_runs(thread_id);
            CREATE INDEX IF NOT EXISTS idx_thread_messages_thread_id ON thread_messages(thread_id);
            CREATE INDEX IF NOT EXISTS idx_thread_attachments_thread_id ON thread_attachments(thread_id);
            CREATE INDEX IF NOT EXISTS idx_knowledge_cards_project_id ON knowledge_cards(project_id);
            CREATE INDEX IF NOT EXISTS idx_knowledge_drafts_project_id ON knowledge_drafts(project_id);
            CREATE INDEX IF NOT EXISTS idx_workflow_runs_thread_id ON workflow_runs(thread_id);
            CREATE INDEX IF NOT EXISTS idx_workflow_stage_runs_run_id ON workflow_stage_runs(workflow_run_id);
        ",
    },
    Migration {
        version: 10,
        sql: "SELECT 1;",
    },
    Migration {
        version: 11,
        sql: "
            UPDATE projects
            SET default_branch = 'main'
            WHERE default_branch IS NULL OR default_branch = '';

            UPDATE tasks
            SET updated_at = COALESCE(created_at, datetime('now'))
            WHERE updated_at IS NULL OR updated_at = '';

            UPDATE sessions
            SET status_message = ''
            WHERE status_message IS NULL;

            UPDATE sessions
            SET tab_label = branch_name
            WHERE tab_label IS NULL OR tab_label = '';

            UPDATE sessions
            SET last_activity_at = COALESCE(created_at, datetime('now'))
            WHERE last_activity_at IS NULL OR last_activity_at = '';

            UPDATE sessions
            SET claude_progress = ''
            WHERE claude_progress IS NULL;
        ",
    },
    Migration {
        version: 12,
        sql: "ALTER TABLE github_items ADD COLUMN label_colors TEXT NOT NULL DEFAULT '{}';",
    },
    Migration {
        version: 13,
        sql: "ALTER TABLE github_projects_v2 ADD COLUMN node_id TEXT;",
    },
    Migration {
        version: 14,
        sql: "
            ALTER TABLE github_items ADD COLUMN linked_pr_number INTEGER;
            ALTER TABLE github_items ADD COLUMN linked_pr_item_id TEXT;
        ",
    },
];

pub struct Store {
    conn: Connection,
}

#[cfg(test)]
impl Store {
    /// Create an in-memory store without running migrations.
    /// Used to test the migration system itself.
    fn open_unmigrated() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(Store { conn })
    }
}

impl Store {
    pub fn open() -> Result<Self> {
        let db_path = config::db_path()?;
        Self::open_at(&db_path)
    }

    /// Open a database at a specific path. Useful for testing with temp files.
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(Store { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Quick sanity check: run `SELECT 1` to prove the DB is accessible.
    pub fn health_check(&self) -> Result<()> {
        let result: i64 = self.conn.query_row("SELECT 1", [], |row| row.get(0))?;
        anyhow::ensure!(result == 1, "health check query returned unexpected value");
        Ok(())
    }

    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
        )?;

        let current_version: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        for migration in MIGRATIONS.iter().filter(|m| m.version > current_version) {
            self.conn.execute_batch("BEGIN")?;
            let result = (|| -> Result<()> {
                if !self.apply_compat_migration(migration)? {
                    self.conn.execute_batch(migration.sql)?;
                }
                self.record_schema_version(migration.version)?;
                Ok(())
            })();
            match result {
                Ok(()) => self.conn.execute_batch("COMMIT")?,
                Err(e) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(e)
                        .with_context(|| format!("migration v{} failed", migration.version));
                }
            }
        }

        Ok(())
    }

    fn record_schema_version(&self, version: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO schema_version (version) VALUES (?1)",
            rusqlite::params![version],
        )?;
        Ok(())
    }

    fn table_has_column(&self, table: &str, column: &str) -> Result<bool> {
        let pragma = format!("PRAGMA table_info({table})");
        let mut stmt = self.conn.prepare(&pragma)?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(columns.iter().any(|candidate| candidate == column))
    }

    fn ensure_column(&self, table: &str, column: &str, sql: &str) -> Result<()> {
        if !self.table_has_column(table, column)? {
            self.conn.execute_batch(sql)?;
        }
        Ok(())
    }

    fn apply_compat_migration(&self, migration: &Migration) -> Result<bool> {
        match migration.version {
            6 if self.table_has_column("tasks", "base")? => {
                if self.table_has_column("tasks", "branch")? {
                    self.conn.execute(
                        "UPDATE tasks
                         SET base = COALESCE(base, branch), branch = NULL
                         WHERE branch IS NOT NULL",
                        [],
                    )?;
                }
                Ok(true)
            }
            7 if self.table_has_column("sessions", "claude_session_id")? => Ok(true),
            13 if self.table_has_column("github_projects_v2", "node_id")? => Ok(true),
            14 if self.table_has_column("github_items", "linked_pr_number")? => Ok(true),
            8 if self.table_has_column("projects", "is_git_linked")? => Ok(true),
            10 => {
                self.ensure_column(
                    "projects",
                    "default_branch",
                    "ALTER TABLE projects ADD COLUMN default_branch TEXT NOT NULL DEFAULT 'main';",
                )?;
                self.ensure_column(
                    "tasks",
                    "updated_at",
                    "ALTER TABLE tasks ADD COLUMN updated_at TEXT NOT NULL DEFAULT '';",
                )?;
                self.ensure_column(
                    "sessions",
                    "tab_label",
                    "ALTER TABLE sessions ADD COLUMN tab_label TEXT NOT NULL DEFAULT '';",
                )?;
                self.ensure_column(
                    "sessions",
                    "last_activity_at",
                    "ALTER TABLE sessions ADD COLUMN last_activity_at TEXT NOT NULL DEFAULT '';",
                )?;
                self.ensure_column(
                    "sessions",
                    "claude_progress",
                    "ALTER TABLE sessions ADD COLUMN claude_progress TEXT NOT NULL DEFAULT '';",
                )?;

                self.conn.execute(
                    "UPDATE projects
                     SET default_branch = 'main'
                     WHERE default_branch IS NULL OR default_branch = ''",
                    [],
                )?;
                self.conn.execute(
                    "UPDATE tasks
                     SET updated_at = COALESCE(NULLIF(updated_at, ''), created_at, datetime('now'))
                     WHERE updated_at IS NULL OR updated_at = ''",
                    [],
                )?;
                self.conn.execute(
                    "UPDATE sessions
                     SET tab_label = COALESCE(NULLIF(tab_label, ''), branch_name)
                     WHERE tab_label IS NULL OR tab_label = ''",
                    [],
                )?;
                self.conn.execute(
                    "UPDATE sessions
                     SET last_activity_at = COALESCE(NULLIF(last_activity_at, ''), created_at, datetime('now'))
                     WHERE last_activity_at IS NULL OR last_activity_at = ''",
                    [],
                )?;
                self.conn.execute(
                    "UPDATE sessions
                     SET claude_progress = ''
                     WHERE claude_progress IS NULL",
                    [],
                )?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_fresh_database() {
        let store = Store::open_unmigrated().unwrap();
        store.migrate().unwrap();

        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);

        // Should be able to insert and query
        store
            .create_project("test", "/tmp/test", "main", true)
            .unwrap();
        assert_eq!(store.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn migrate_is_idempotent() {
        let store = Store::open_unmigrated().unwrap();
        store.migrate().unwrap();
        // Running migrate again should be a no-op
        store.migrate().unwrap();

        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);
    }

    /// Run each migration step by step and verify
    /// the schema version advances correctly after each one.
    #[test]
    fn migrate_sequential_upgrade() {
        let store = Store::open_unmigrated().unwrap();
        store
            .conn
            .execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
            .unwrap();

        for migration in MIGRATIONS {
            // Simulate applying one migration at a time
            store.conn.execute_batch("BEGIN").unwrap();
            store.conn.execute_batch(migration.sql).unwrap();
            let current: i64 = store
                .conn
                .query_row(
                    "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if current == 0 {
                store
                    .conn
                    .execute(
                        "INSERT INTO schema_version (version) VALUES (?1)",
                        rusqlite::params![migration.version],
                    )
                    .unwrap();
            } else {
                store
                    .conn
                    .execute(
                        "UPDATE schema_version SET version = ?1",
                        rusqlite::params![migration.version],
                    )
                    .unwrap();
            }
            store.conn.execute_batch("COMMIT").unwrap();

            let version: i64 = store
                .conn
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(version, migration.version);
        }

        // Verify final version matches latest
        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);
    }

    #[test]
    fn migrate_tolerates_partially_applied_base_column() {
        let store = Store::open_unmigrated().unwrap();
        store
            .conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
                INSERT INTO schema_version (version) VALUES (5);
                CREATE TABLE tasks (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'pending',
                    mode TEXT NOT NULL DEFAULT 'supervised',
                    session_id TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    started_at TEXT,
                    completed_at TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    sort_order INTEGER NOT NULL DEFAULT 0,
                    pr_url TEXT,
                    branch TEXT,
                    push_mode TEXT NOT NULL DEFAULT 'pr',
                    ci_status TEXT,
                    review_loop INTEGER NOT NULL DEFAULT 0,
                    base TEXT
                );
                CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    branch_name TEXT NOT NULL,
                    worktree_path TEXT NOT NULL,
                    tab_label TEXT NOT NULL,
                    claude_status TEXT NOT NULL DEFAULT 'idle',
                    status_message TEXT NOT NULL DEFAULT '',
                    last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
                    files_changed INTEGER NOT NULL DEFAULT 0,
                    lines_added INTEGER NOT NULL DEFAULT 0,
                    lines_removed INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    closed_at TEXT,
                    claude_progress TEXT NOT NULL DEFAULT ''
                );
                CREATE TABLE projects (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    repo_path TEXT NOT NULL UNIQUE,
                    default_branch TEXT NOT NULL DEFAULT 'main',
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                INSERT INTO tasks (id, project_id, title, branch, base, push_mode, review_loop)
                VALUES ('task-1', 'project-1', 'Task', 'feature/base', NULL, 'pr', 0);
                ",
            )
            .unwrap();

        store.migrate().unwrap();

        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);

        let (base, branch): (Option<String>, Option<String>) = store
            .conn
            .query_row(
                "SELECT base, branch FROM tasks WHERE id = 'task-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(base.as_deref(), Some("feature/base"));
        assert!(branch.is_none());
    }

    #[test]
    fn migrate_tolerates_historical_schema_versions() {
        let store = Store::open_unmigrated().unwrap();
        store
            .conn
            .execute_batch(
                "
                CREATE TABLE schema_version (
                    version INTEGER PRIMARY KEY
                );
                INSERT INTO schema_version (version) VALUES (1);
                INSERT INTO schema_version (version) VALUES (2);
                INSERT INTO schema_version (version) VALUES (3);
                INSERT INTO schema_version (version) VALUES (4);
                INSERT INTO schema_version (version) VALUES (5);
                CREATE TABLE projects (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    repo_path TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    branch_name TEXT NOT NULL,
                    worktree_path TEXT NOT NULL,
                    claude_status TEXT NOT NULL DEFAULT 'idle',
                    status_message TEXT,
                    files_changed INTEGER NOT NULL DEFAULT 0,
                    lines_added INTEGER NOT NULL DEFAULT 0,
                    lines_removed INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    closed_at TEXT,
                    claude_session_id TEXT
                );
                CREATE TABLE tasks (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    description TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'pending',
                    mode TEXT NOT NULL DEFAULT 'supervised',
                    session_id TEXT,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    started_at TEXT,
                    completed_at TEXT,
                    input_tokens INTEGER NOT NULL DEFAULT 0,
                    output_tokens INTEGER NOT NULL DEFAULT 0,
                    sort_order INTEGER NOT NULL DEFAULT 0,
                    pr_url TEXT,
                    branch TEXT,
                    push_mode TEXT NOT NULL DEFAULT 'pr',
                    ci_status TEXT,
                    review_loop INTEGER NOT NULL DEFAULT 0,
                    base TEXT
                );
                INSERT INTO projects (id, name, repo_path)
                VALUES ('project-1', 'Project', '/tmp/project-1');
                INSERT INTO sessions (
                    id, project_id, branch_name, worktree_path, claude_status, status_message
                ) VALUES ('session-1', 'project-1', 'feature/base', '/tmp/project-1-wt', 'idle', NULL);
                INSERT INTO tasks (id, project_id, title, branch, push_mode, review_loop)
                VALUES ('task-1', 'project-1', 'Task', 'feature/base', 'pr', 0);
                ",
            )
            .unwrap();

        store.migrate().unwrap();

        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);

        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM schema_version WHERE version = ?1",
                rusqlite::params![6],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let project = store.get_project("project-1").unwrap();
        assert_eq!(project.default_branch, "main");

        let session = store.get_session("session-1").unwrap();
        assert_eq!(session.tab_label, "feature/base");
        assert_eq!(session.last_activity_at, session.created_at);
        assert_eq!(session.status_message, "");

        let task = store.get_task("task-1").unwrap();
        assert_eq!(task.updated_at, task.created_at);
    }

    /// Verify the schema after all migrations contains the expected tables and columns.
    /// This catches accidental column removals or renames that would break row mappers.
    #[test]
    fn schema_has_expected_tables_and_columns() {
        let store = Store::open_unmigrated().unwrap();
        store.migrate().unwrap();

        // Check all expected tables exist
        let tables: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        let expected_tables = [
            "external_sessions",
            "github_comment_cache",
            "github_issue_cache",
            "github_items",
            "github_pr_cache",
            "github_projects_v2",
            "github_repos",
            "github_review_cache",
            "github_review_comment_cache",
            "knowledge_cards",
            "knowledge_drafts",
            "knowledge_links",
            "projects",
            "rate_limit_state",
            "schema_version",
            "sessions",
            "subtasks",
            "tasks",
            "thread_attachments",
            "thread_messages",
            "thread_runs",
            "thread_runtime_state",
            "threads",
            "workflow_artifacts",
            "workflow_defs",
            "workflow_runs",
            "workflow_stage_runs",
        ];
        for table in &expected_tables {
            assert!(
                tables.contains(&(*table).to_string()),
                "missing table: {table}"
            );
        }

        // Check critical columns exist in tasks table (covers all migration-added columns)
        let task_columns: Vec<String> = {
            let mut stmt = store.conn.prepare("PRAGMA table_info(tasks)").unwrap();
            stmt.query_map([], |row| row.get(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        let expected_task_columns = [
            "id",
            "project_id",
            "title",
            "description",
            "status",
            "mode",
            "session_id",
            "created_at",
            "updated_at",
            "started_at",
            "completed_at",
            "input_tokens",
            "output_tokens",
            "sort_order",
            "pr_url",
            // Added by migrations v3-v6:
            "branch",
            "push_mode",
            "ci_status",
            "review_loop",
            "base",
        ];
        for col in &expected_task_columns {
            assert!(
                task_columns.contains(&(*col).to_string()),
                "tasks table missing column: {col}"
            );
        }

        // Check projects table has the v8 column
        let project_columns: Vec<String> = {
            let mut stmt = store.conn.prepare("PRAGMA table_info(projects)").unwrap();
            stmt.query_map([], |row| row.get(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        assert!(
            project_columns.contains(&"is_git_linked".to_string()),
            "projects table missing column: is_git_linked"
        );

        // Check sessions table has the v7 column
        let session_columns: Vec<String> = {
            let mut stmt = store.conn.prepare("PRAGMA table_info(sessions)").unwrap();
            stmt.query_map([], |row| row.get(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        let expected_session_columns = [
            "id",
            "project_id",
            "branch_name",
            "worktree_path",
            "tab_label",
            "claude_status",
            "status_message",
            "last_activity_at",
            "files_changed",
            "lines_added",
            "lines_removed",
            "created_at",
            "closed_at",
            "claude_progress",
            // Added by migration v7:
            "claude_session_id",
        ];
        for col in &expected_session_columns {
            assert!(
                session_columns.contains(&(*col).to_string()),
                "sessions table missing column: {col}"
            );
        }
    }

    /// Verify migration versions are sequential and non-duplicated.
    #[test]
    #[expect(clippy::cast_possible_wrap, reason = "migration count is tiny")]
    fn migration_versions_are_sequential() {
        for (i, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                migration.version,
                (i as i64) + 1,
                "migration at index {i} has version {} but expected {}",
                migration.version,
                i + 1,
            );
        }
    }

    /// Verify `row_to_task` maps all columns correctly by round-tripping through
    /// `create_task` + `get_task`. If a column is added to the schema but not to
    /// `TASK_COLUMNS` or `row_to_task`, this will fail with a column index error.
    #[test]
    fn task_row_mapper_covers_all_columns() {
        let store = Store::open_unmigrated().unwrap();
        store.migrate().unwrap();

        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();

        // Create a task with every optional field populated
        let task = store
            .create_task(
                &project.id,
                "mapper-test",
                "desc",
                super::TaskMode::Autonomous,
                Some("feat/x"),
                Some("develop"),
                super::PushMode::Push,
                true,
            )
            .unwrap();

        // Verify every field was set and round-trips correctly
        let fetched = store.get_task(&task.id).unwrap();
        assert_eq!(fetched.title, "mapper-test");
        assert_eq!(fetched.description, "desc");
        assert_eq!(fetched.mode, super::TaskMode::Autonomous);
        assert_eq!(fetched.branch.as_deref(), Some("feat/x"));
        assert_eq!(fetched.base.as_deref(), Some("develop"));
        assert_eq!(fetched.push_mode, super::PushMode::Push);
        assert!(fetched.review_loop);
        assert_eq!(fetched.status, super::TaskStatus::Pending);
        assert!(fetched.ci_status.is_none());

        // Also verify the task columns match by checking the column count in the schema
        let col_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tasks')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // tasks table should have 20 columns after all migrations
        assert_eq!(
            col_count, 20,
            "tasks table column count changed — update TASK_COLUMNS and row_to_task"
        );
    }

    /// Verify that `open_in_memory()` produces a DB where all CRUD operations work.
    /// This is a smoke test for the full migration + initial data path.
    #[test]
    fn open_in_memory_is_fully_functional() {
        let store = Store::open_in_memory().unwrap();

        // Verify health check works
        store.health_check().unwrap();

        // Full CRUD cycle
        let project = store
            .create_project("test", "/tmp/test", "main", true)
            .unwrap();
        let task = store
            .create_task(
                &project.id,
                "task",
                "desc",
                super::TaskMode::Supervised,
                None,
                None,
                super::PushMode::Pr,
                false,
            )
            .unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab")
            .unwrap();
        let subtask = store.create_subtask(&task.id, "step", "").unwrap();

        // Read back
        assert_eq!(store.list_projects().unwrap().len(), 1);
        assert_eq!(store.list_tasks_for_project(&project.id).unwrap().len(), 1);
        assert_eq!(
            store
                .list_active_sessions_for_project(&project.id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.list_subtasks_for_task(&task.id).unwrap().len(), 1);

        // Rate limit state exists (singleton)
        let state = store.get_rate_limit_state().unwrap();
        assert!(!state.is_rate_limited);

        // Clean up
        store.delete_subtask(&subtask.id).unwrap();
        store.close_session(&session.id).unwrap();
        store.delete_task(&task.id).unwrap();
        store.delete_project(&project.id).unwrap();
        assert_eq!(store.list_projects().unwrap().len(), 0);
    }
}
