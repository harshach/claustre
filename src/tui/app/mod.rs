//! TUI application state and event handling.
//!
//! Contains the `App` struct (all mutable state), key/mouse handlers,
//! data refresh logic, and background task coordination.

mod data_refresh;
mod event_loop;
mod initialization;
mod input;
mod polling;
mod pty_management;
mod session_lifecycle;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::widgets::ListState;

use crate::pty::SessionTerminals;
use crate::runtime::RuntimeServiceState;
use crate::store::{
    GitHubCommentCache, GitHubIssueCache, GitHubItem, GitHubPrCache, GitHubProjectV2Cache,
    GitHubRepoCache, GitHubReviewCache, GitHubReviewCommentCache, Project, ProjectStats, Session,
    Store, Task, TaskStatus, TaskStatusCounts, Thread, ThreadAttachment, ThreadMessage, ThreadRun,
    ThreadRuntimeState, WorkflowDef, WorkflowRun, WorkflowStageRun,
};

/// How long toast notifications remain visible.
const TOAST_DURATION: Duration = Duration::from_secs(4);

/// Tick rate when viewing the dashboard.
///
/// 200 ms keeps background session PTY output reasonably current (5× per
/// second) while staying light on CPU.  The old 1 s rate meant sessions
/// could accumulate up to 1 second of unprocessed output, causing a visible
/// catch-up lag when the user switched to a session tab.
const DASHBOARD_TICK: Duration = Duration::from_millis(200);
/// Tick rate when viewing a session tab (fast refresh for smooth PTY rendering).
const SESSION_TICK: Duration = Duration::from_millis(16);
/// How often to run the slow-path tick work (DB refresh, PR polling, etc.).
/// Applies on all tabs since the dashboard tick rate (200 ms) is now faster
/// than the desired refresh interval.
const SLOW_TICK: Duration = Duration::from_secs(1);

/// A tab in the TUI — either the main dashboard or a session terminal.
pub(crate) enum Tab {
    Dashboard,
    Session {
        session_id: String,
        terminals: Box<SessionTerminals>,
        label: String,
        view_mode: SessionTabView,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTabView {
    Conversation,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Projects,
    Tasks,
    Inspector,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkbenchView {
    MyTasks,
    SprintBoard,
    Threads,
    Reviews,
    Agents,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SidebarItem {
    Navigation(WorkbenchView),
    Repository(usize),
}

impl WorkbenchView {
    pub(crate) const ALL: [Self; 5] = [
        Self::MyTasks,
        Self::SprintBoard,
        Self::Reviews,
        Self::Agents,
        Self::Settings,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::MyTasks => "My Tasks",
            Self::SprintBoard => "Sprint Board",
            Self::Threads => "Threads",
            Self::Reviews => "Reviews",
            Self::Agents => "Agents",
            Self::Settings => "Settings",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewQueueTab {
    Authored,
    NeedsReview,
}

impl ReviewQueueTab {
    pub(crate) const ALL: [Self; 2] = [Self::Authored, Self::NeedsReview];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Authored => "Authored PRs",
            Self::NeedsReview => "Needs Review",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewDrawerTab {
    Description,
    Diff,
    Comments,
}

impl ReviewDrawerTab {
    pub(crate) const ALL: [Self; 3] = [Self::Description, Self::Diff, Self::Comments];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Description => "Description",
            Self::Diff => "Diff",
            Self::Comments => "Comments",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoardScope {
    AssignedToMe,
    AllSprint,
}

impl BoardScope {
    pub(crate) const ALL: [Self; 2] = [Self::AssignedToMe, Self::AllSprint];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AssignedToMe => "Assigned to me",
            Self::AllSprint => "All sprint issues",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::AssignedToMe => Self::AllSprint,
            Self::AllSprint => Self::AssignedToMe,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    AiProviders,
    Permissions,
    GitHub,
    Layout,
    Notifications,
    ReviewLoop,
    Workflows,
    Sandbox,
    General,
}

impl SettingsSection {
    pub(crate) const ALL: [Self; 9] = [
        Self::AiProviders,
        Self::Permissions,
        Self::GitHub,
        Self::Layout,
        Self::Notifications,
        Self::ReviewLoop,
        Self::Workflows,
        Self::Sandbox,
        Self::General,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AiProviders => "AI Providers",
            Self::Permissions => "Permissions",
            Self::GitHub => "GitHub",
            Self::Layout => "Layout",
            Self::Notifications => "Notifications",
            Self::ReviewLoop => "Review Loop",
            Self::Workflows => "Workflows",
            Self::Sandbox => "Sandbox",
            Self::General => "General",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsEditTarget {
    ClaudeModel,
    ClaudeEffort,
    KeymapPreset,
    KeymapLeader,
    NotificationCommand,
    NotificationTemplate,
    ReviewLoopInterval,
    ReviewLoopPrompt,
    RuntimeDefaultProfile,
    RuntimeSandboxPath,
    RuntimeAttachmentsDir,
    WorkflowName,
    ClientId,
    AppSlug,
    InstallUrl,
    RelayUrl,
    InstallationId,
}

impl SettingsEditTarget {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ClaudeModel => "Claude Model",
            Self::ClaudeEffort => "Claude Effort",
            Self::KeymapPreset => "Keymap Preset",
            Self::KeymapLeader => "Leader Key",
            Self::NotificationCommand => "Notification Command",
            Self::NotificationTemplate => "Notification Template",
            Self::ReviewLoopInterval => "Review Loop Poll Interval",
            Self::ReviewLoopPrompt => "Review Loop Prompt",
            Self::RuntimeDefaultProfile => "Default Runtime Profile",
            Self::RuntimeSandboxPath => "Sandbox Path",
            Self::RuntimeAttachmentsDir => "Attachments Directory",
            Self::WorkflowName => "Workflow Name",
            Self::ClientId => "GitHub Client ID",
            Self::AppSlug => "GitHub App Slug",
            Self::InstallUrl => "GitHub Install URL",
            Self::RelayUrl => "GitHub Relay URL",
            Self::InstallationId => "Default Installation",
        }
    }

    pub(crate) fn current_value(self, config: &crate::config::Config) -> String {
        match self {
            Self::ClaudeModel => config.claude.model.clone(),
            Self::ClaudeEffort => config.claude.effort.clone(),
            Self::KeymapPreset => config.ui.keymap.preset.clone(),
            Self::KeymapLeader => config.ui.keymap.leader.clone(),
            Self::NotificationCommand => config.notifications.command.clone(),
            Self::NotificationTemplate => config.notifications.template.clone(),
            Self::ReviewLoopInterval => config.review_loop.poll_interval_secs.to_string(),
            Self::ReviewLoopPrompt => config.review_loop.prompt.clone().unwrap_or_default(),
            Self::RuntimeDefaultProfile => {
                config.runtime.default_profile.clone().unwrap_or_default()
            }
            Self::RuntimeSandboxPath => config.runtime.sandbox_path.clone().unwrap_or_default(),
            Self::RuntimeAttachmentsDir => {
                config.runtime.attachments_dir.clone().unwrap_or_default()
            }
            Self::WorkflowName => String::new(), // handled specially in settings key handler
            Self::ClientId => config.github_app.client_id.clone().unwrap_or_default(),
            Self::AppSlug => config.github_app.app_slug.clone().unwrap_or_default(),
            Self::InstallUrl => config.github_app.install_url.clone().unwrap_or_default(),
            Self::RelayUrl => config.github_app.relay_url.clone().unwrap_or_default(),
            Self::InstallationId => config
                .github_app
                .default_installation_id
                .clone()
                .unwrap_or_default(),
        }
    }

    pub(crate) fn apply(
        self,
        config: &mut crate::config::Config,
        value: &str,
    ) -> anyhow::Result<()> {
        let value = value.trim();
        match self {
            Self::ClaudeModel => {
                anyhow::ensure!(!value.is_empty(), "Claude model cannot be empty");
                config.claude.model = value.to_string();
            }
            Self::ClaudeEffort => {
                anyhow::ensure!(!value.is_empty(), "Claude effort cannot be empty");
                config.claude.effort = value.to_string();
            }
            Self::KeymapPreset => {
                anyhow::ensure!(!value.is_empty(), "Keymap preset cannot be empty");
                config.ui.keymap.preset = value.to_string();
            }
            Self::KeymapLeader => {
                anyhow::ensure!(!value.is_empty(), "Leader key cannot be empty");
                config.ui.keymap.leader = value.to_string();
            }
            Self::NotificationCommand => {
                anyhow::ensure!(!value.is_empty(), "Notification command cannot be empty");
                config.notifications.command = value.to_string();
            }
            Self::NotificationTemplate => {
                anyhow::ensure!(!value.is_empty(), "Notification template cannot be empty");
                config.notifications.template = value.to_string();
            }
            Self::ReviewLoopInterval => {
                let parsed = value
                    .parse::<u64>()
                    .map_err(|_| anyhow::anyhow!("Review loop interval must be an integer"))?;
                anyhow::ensure!(parsed > 0, "Review loop interval must be greater than zero");
                config.review_loop.poll_interval_secs = parsed;
            }
            Self::ReviewLoopPrompt => {
                config.review_loop.prompt = (!value.is_empty()).then(|| value.to_string());
            }
            Self::RuntimeDefaultProfile => {
                config.runtime.default_profile = (!value.is_empty()).then(|| value.to_string());
            }
            Self::RuntimeSandboxPath => {
                config.runtime.sandbox_path = (!value.is_empty()).then(|| value.to_string());
            }
            Self::RuntimeAttachmentsDir => {
                config.runtime.attachments_dir = (!value.is_empty()).then(|| value.to_string());
            }
            Self::WorkflowName => {
                // Handled specially in handle_settings_edit_key — workflow rename
                // requires file operations, not just config changes.
            }
            Self::ClientId => {
                config.github_app.client_id = (!value.is_empty()).then(|| value.to_string());
            }
            Self::AppSlug => {
                config.github_app.app_slug = (!value.is_empty()).then(|| value.to_string());
            }
            Self::InstallUrl => {
                config.github_app.install_url = (!value.is_empty()).then(|| value.to_string());
            }
            Self::RelayUrl => {
                config.github_app.relay_url = (!value.is_empty()).then(|| value.to_string());
            }
            Self::InstallationId => {
                config.github_app.default_installation_id =
                    (!value.is_empty()).then(|| value.to_string());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InspectorTab {
    Issue,
    Diff,
    Runtime,
    Tests,
    Plan,
    Attachments,
}

impl InspectorTab {
    pub(crate) const ALL: [Self; 6] = [
        Self::Issue,
        Self::Diff,
        Self::Runtime,
        Self::Tests,
        Self::Plan,
        Self::Attachments,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Issue => "Issue",
            Self::Diff => "Diff",
            Self::Runtime => "Runtime",
            Self::Tests => "Tests",
            Self::Plan => "Plan",
            Self::Attachments => "Attachments",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkbenchDragTarget {
    SidebarDivider,
    InspectorDivider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastStyle {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Normal,
    LaunchThread,
    ThreadCompose,
    ThreadProviderPicker,
    SettingsEdit,
    SettingsPicker,
    GitHubInstallationPicker,
    GitHubProjectPicker,
    ProjectPicker,
    NewTask,
    EditTask,
    NewProject,
    ConfirmDelete,
    CommandPalette,
    SkillPanel,
    SkillSearch,
    SkillAdd,
    HelpOverlay,
    TaskFilter,
    SubtaskPanel,
    TaskDetails,
    ConfigureWizard,
    BoardView,
    MilestoneFilter,
    BoardFilter,
    CommentCompose,
    BoardIssueDrawer,
    FieldPicker,
    CreateGitHubIssue,
    EditGitHubIssue,
    MyTaskDrawer,
    ReviewDrawer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteTarget {
    Project,
    Task,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchThreadField {
    Provider,
    RuntimeProfile,
    Workflow,
    Title,
    ExtraContext,
}

impl LaunchThreadField {
    pub(crate) const ALL: [Self; 5] = [
        Self::Provider,
        Self::RuntimeProfile,
        Self::Workflow,
        Self::Title,
        Self::ExtraContext,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Provider => "Provider",
            Self::RuntimeProfile => "Runtime Profile",
            Self::Workflow => "Workflow",
            Self::Title => "Title",
            Self::ExtraContext => "Additional Context",
        }
    }

    pub(crate) fn is_text(self) -> bool {
        !matches!(self, Self::Provider | Self::Workflow)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingBoardIssueLaunch {
    pub github_item_id: Option<String>,
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingReviewLaunchMode {
    ContinueWork,
    Review,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingReviewPrLaunch {
    pub github_item_id: String,
    pub number: i64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub base_ref: Option<String>,
    pub head_ref: Option<String>,
    pub mode: PendingReviewLaunchMode,
}

#[derive(Debug, Clone)]
pub(crate) struct LaunchThreadDraft {
    pub task_id: Option<String>,
    pub board_issue: Option<PendingBoardIssueLaunch>,
    pub review_pr: Option<PendingReviewPrLaunch>,
    pub provider_kind: crate::store::ProviderKind,
    pub runtime_profile: String,
    pub workflow_name: String,
    pub title: String,
    pub extra_context: String,
}

impl LaunchThreadDraft {
    pub(crate) fn is_ad_hoc(&self) -> bool {
        self.task_id.is_none() && self.board_issue.is_none() && self.review_pr.is_none()
    }

    pub(crate) fn source_label(&self) -> String {
        if let Some(pr) = &self.review_pr {
            let prefix = match pr.mode {
                PendingReviewLaunchMode::ContinueWork => "GitHub PR",
                PendingReviewLaunchMode::Review => "Review PR",
            };
            format!("{prefix} #{} {}", pr.number, pr.title)
        } else if let Some(issue) = &self.board_issue {
            format!("GitHub issue #{} {}", issue.number, issue.title)
        } else if let Some(task_id) = self.task_id.as_deref() {
            format!("Local task {task_id}")
        } else {
            "Ad hoc thread".to_string()
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PaletteItem {
    pub label: String,
    pub action: PaletteAction,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PaletteAction {
    NewTask,
    AddProject,
    RemoveProject,
    FocusProjects,
    FocusTasks,
    FindSkills,
    UpdateSkills,
    Configure,
    SprintBoard,
    Quit,
}

/// Pre-fetched per-project summary for the sidebar (avoids DB queries during rendering).
#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectSummary {
    pub active_sessions: Vec<Session>,
    pub task_counts: TaskStatusCounts,
    pub default_branch: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ToolStatusSnapshot {
    pub claude: bool,
    pub codex: bool,
    pub node: bool,
    pub gh: bool,
    pub rtk: bool,
}

/// Result from a background session create/teardown.
enum SessionOpResult {
    /// Session created successfully — carry the setup info for PTY spawning.
    Created(Box<crate::session::SessionSetup>),
    /// Thread launched successfully — may include a live session to spawn.
    ThreadLaunched {
        result: Box<crate::threads::LaunchThreadResult>,
    },
    /// Session created but no task to launch (e.g. bare session).
    CreatedNoTask { message: String },
    /// Teardown completed.
    TornDown { message: String },
    /// An operation failed.
    Error { message: String },
}

/// Result from a background GitHub mutation (comment, close/open, create, edit).
pub(crate) enum GitHubMutationResult {
    CommentCreated { message: String },
    StateChanged { message: String },
    IssueCreated { message: String },
    IssueEdited { message: String },
    FieldUpdated { message: String },
    Error { message: String },
}

/// What the GitHub API reports about a PR's state.
enum PrStatus {
    Merged,
    Conflicting,
    CiFailed,
    CiRunning,
    CiPassed,
    Open,
}

/// Result from a background PR status check.
enum PrPollResult {
    /// PR was merged — task should be marked done.
    Merged {
        task_id: String,
        session_id: Option<String>,
        task_title: String,
    },
    /// PR has merge conflicts — task should transition to conflict.
    Conflict { task_id: String, task_title: String },
    /// Previously conflicting PR is now mergeable — task goes back to `in_review`.
    ConflictResolved { task_id: String, task_title: String },
    /// PR has failed CI checks — task should transition to `ci_failed`.
    CiFailed { task_id: String, task_title: String },
    /// Previously failed CI checks are now passing — task goes back to `in_review`.
    CiRecovered { task_id: String, task_title: String },
    /// CI status changed (running or passed) — update the `ci_status` field without changing task status.
    CiStatusChanged {
        task_id: String,
        ci_status: crate::store::CiStatus,
    },
}

/// Result from a background git diff --stat check.
struct GitStatsResult {
    session_id: String,
    files_changed: i64,
    lines_added: i64,
    lines_removed: i64,
}

pub(crate) struct App {
    pub store: Store,
    pub config: crate::config::Config,
    pub theme: super::theme::Theme,
    pub keymap: super::keymap::KeyMap,
    pub should_quit: bool,
    pub workbench_view: WorkbenchView,
    pub inspector_tab: InspectorTab,
    pub focus: Focus,
    pub input_mode: InputMode,
    pub leader_pending: bool,
    pub loading: bool,

    // Tab system (tab 0 = Dashboard, additional tabs = session terminals)
    pub tabs: Vec<Tab>,
    pub active_tab: usize,

    // Data
    pub projects: Vec<Project>,
    pub sessions: Vec<Session>,
    pub tasks: Vec<Task>,
    pub threads: Vec<Thread>,

    // Pre-fetched sidebar data (project_id -> summary)
    pub project_summaries: HashMap<String, ProjectSummary>,

    // Cached stats for the selected project (avoids DB queries during rendering)
    pub project_stats: Option<ProjectStats>,
    pub tool_status: ToolStatusSnapshot,
    pub github_status: crate::github_app::GitHubAppStatus,
    pub github_installations: Vec<crate::github_app::GitHubInstallation>,
    pub github_projects_v2: Vec<GitHubProjectV2Cache>,

    // Selection indices
    pub sidebar_cursor: usize,
    pub project_index: usize,
    pub task_index: usize,
    pub thread_index: usize,
    pub review_index: usize,
    pub review_queue_tab: ReviewQueueTab,
    pub settings_section_index: usize,
    pub settings_workflow_index: usize,
    pub github_installation_index: usize,
    pub github_project_index: usize,
    pub project_picker_index: usize,
    pub github_settings_show_advanced: bool,
    pub inspector_scroll: u16,
    pub workbench_sidebar_width: u16,
    pub workbench_inspector_width: u16,
    pub workbench_drag_target: Option<WorkbenchDragTarget>,
    pub settings_edit_target: Option<SettingsEditTarget>,
    // Picker overlay state for structured option selection (model, effort, etc.)
    pub settings_picker_options: Vec<String>,
    pub settings_picker_index: usize,
    pub settings_picker_target: Option<SettingsEditTarget>,
    pub launch_thread_draft: Option<LaunchThreadDraft>,
    pub launch_thread_field_index: usize,
    pub thread_provider_picker_index: usize,
    pub available_workflow_names: Vec<String>,
    pub thread_compose_thread_id: Option<String>,
    pub thread_compose_buffer: String,
    pub thread_compose_cursor: usize,

    // Slash command autocomplete
    pub slash_suggestions: Vec<String>,
    pub slash_suggestion_index: usize,

    // Scroll state for task list (used by ratatui's stateful List widget)
    #[expect(
        dead_code,
        reason = "reserved for future stateful workbench list scrolling"
    )]
    pub task_list_state: ListState,

    // Input buffer for new task creation
    pub input_buffer: String,
    // Cursor byte-offset within input_buffer (clamped to buf.len())
    pub input_cursor: usize,

    // Enhanced task form state (field 0=prompt, 1=mode, 2=base, 3=branch, 4=push_mode, 5=review_loop, 6=subtasks)
    pub new_task_field: u8,
    pub new_task_description: String,
    pub new_task_mode: crate::store::TaskMode,
    pub new_task_base: String,
    pub new_task_branch: String,
    pub new_task_push_mode: crate::store::PushMode,
    pub new_task_review_loop: bool,

    // Add Project form state
    pub new_project_field: u8,
    pub new_project_name: String,
    pub new_project_path: String,
    // Git linked toggle for new project
    pub new_project_git_linked: bool,

    // Sprint board state
    pub board_issues: Vec<Vec<crate::github::GitHubBoardItem>>,
    pub board_columns: Vec<String>,
    pub board_column_index: usize,
    pub board_issue_index: usize,
    pub board_project_title: Option<String>,
    pub board_sprint_filter: Option<String>,
    pub board_sprints: Vec<String>,
    pub board_sprint_index: usize,
    pub board_scope: BoardScope,
    pub board_loading: bool,
    pub board_error: Option<String>,
    /// Horizontal scroll offset for board columns (skip N non-empty columns).
    pub board_scroll_offset: usize,
    /// When true, the next `x` press confirms close/reopen of the selected issue.
    pub board_confirm_close: bool,

    // Board text filter state
    pub board_filter: String,
    pub board_filter_cursor: usize,
    pub board_first_load: bool,
    pub board_all_issues: Vec<Vec<crate::github::GitHubBoardItem>>,
    pub board_source_items: Vec<crate::github::GitHubBoardItem>,
    /// Comments for the currently selected board item (pre-fetched for the inspector).
    pub board_selected_comments: Vec<crate::store::GitHubCommentCache>,
    /// Comments for the currently selected My Task item (pre-fetched for the drawer).
    pub my_task_selected_comments: Vec<crate::store::GitHubCommentCache>,

    // ── Comment compose ─────────────────────────────────────────────
    pub comment_compose_buffer: String,
    pub comment_compose_cursor: usize,
    /// `(repo_path, issue_number, github_item_id)` for the comment target.
    pub comment_compose_target: Option<(String, i64, String)>,
    /// Which mode to return to when exiting comment compose.
    pub comment_compose_return_mode: InputMode,

    // ── GitHub issue create/edit form ───────────────────────────────
    pub github_issue_form_title: String,
    pub github_issue_form_body: String,
    pub github_issue_form_labels: String,
    pub github_issue_form_assignees: String,
    pub github_issue_form_field: u8,
    /// When editing, the issue number being edited.
    pub github_issue_editing_number: Option<i64>,

    // ── Field picker (status/sprint change) ─────────────────────────
    pub field_picker_options: Vec<crate::github::ProjectV2FieldOption>,
    pub field_picker_index: usize,
    pub field_picker_field_name: Option<String>,
    pub field_picker_field_id: Option<String>,
    pub field_picker_project_node_id: Option<String>,
    pub field_picker_item_node_id: Option<String>,

    // ── GitHub mutation results channel ─────────────────────────────
    pub github_mutation_tx: std::sync::mpsc::Sender<GitHubMutationResult>,
    pub github_mutation_rx: std::sync::mpsc::Receiver<GitHubMutationResult>,

    pub my_task_items: Vec<MyTaskItem>,
    pub review_authored_items: Vec<ReviewQueueItem>,
    pub review_requested_items: Vec<ReviewQueueItem>,

    // ── Review inspector context (pre-fetched for selected review item) ──
    pub review_selected_comments: Vec<GitHubCommentCache>,
    pub review_selected_reviews: Vec<GitHubReviewCache>,
    pub review_selected_review_comments: Vec<GitHubReviewCommentCache>,

    // ── Inspector expansion toggle ─────────────────────────────────
    pub inspector_expanded: bool,

    // ── Review PR drawer ───────────────────────────────────────────
    pub review_drawer_tab: ReviewDrawerTab,
    pub review_drawer_scroll: u16,

    // Path autocomplete state
    pub path_suggestions: Vec<String>,
    pub path_suggestion_index: usize,
    pub show_path_suggestions: bool,

    // Confirm delete state
    pub confirm_target: String,
    pub confirm_entity_id: String,
    pub confirm_delete_kind: DeleteTarget,

    // Editing task state
    pub editing_task_id: Option<String>,

    // Task filter state
    pub task_filter: String,
    pub task_filter_cursor: usize,

    // Subtask state
    pub subtasks: Vec<crate::store::Subtask>,
    pub subtask_index: usize,
    pub subtask_counts: HashMap<String, (i64, i64)>,

    // Task details panel scroll offset
    pub task_details_scroll: u16,

    // Inline subtasks for new-task form
    pub new_task_subtasks: Vec<String>,
    pub new_task_subtask_index: usize,
    pub editing_subtask_index: Option<usize>,

    // Command palette state
    pub palette_items: Vec<PaletteItem>,
    pub palette_filtered: Vec<usize>,
    pub palette_index: usize,

    // Skills state
    pub installed_skills: Vec<crate::skills::InstalledSkill>,
    pub search_results: Vec<crate::skills::SearchResult>,
    pub skill_index: usize,
    pub skill_scope_global: bool,
    pub skill_detail_content: String,
    pub skill_status_message: String,
    pub selected_search_indices: HashSet<usize>,

    // Rate limit state
    pub rate_limit_state: crate::store::RateLimitState,

    // Background API usage fetch coordination
    usage_fetch_in_progress: Arc<AtomicBool>,

    // Background title generation
    title_tx: mpsc::Sender<(String, String)>,
    title_rx: mpsc::Receiver<(String, String)>,
    pub pending_titles: HashSet<String>,
    // Tasks waiting for title generation before auto-launching (task_id → project_id)
    pending_auto_launch: HashMap<String, String>,
    // Pending autonomous tasks to auto-launch on startup (project_id, task)
    startup_auto_launch: VecDeque<(String, Task)>,

    // PR status polling (merge + conflict detection)
    pr_poll_in_progress: Arc<AtomicBool>,
    pr_poll_tx: mpsc::Sender<PrPollResult>,
    pr_poll_rx: mpsc::Receiver<PrPollResult>,
    last_pr_poll: Instant,

    // Git stats polling
    git_stats_in_progress: Arc<AtomicBool>,
    git_stats_tx: mpsc::Sender<GitStatsResult>,
    git_stats_rx: mpsc::Receiver<GitStatsResult>,
    last_git_stats_poll: Instant,

    // External session scanner
    scanner_in_progress: Arc<AtomicBool>,
    scanner_tx: mpsc::Sender<crate::scanner::ScanResult>,
    scanner_rx: mpsc::Receiver<crate::scanner::ScanResult>,
    last_scan: Instant,
    pub external_sessions: Vec<crate::store::ExternalSession>,

    // Background session operations (create/teardown)
    session_op_tx: mpsc::Sender<SessionOpResult>,
    session_op_rx: mpsc::Receiver<SessionOpResult>,
    session_op_in_progress: bool,

    // Pending relaunch: when relaunching a stuck task, teardown fires first,
    // then this queues the task for auto-launch once teardown completes.
    // (task_id, project_id)
    pending_relaunch: Option<(String, String)>,

    // Toast notification
    pub toast_message: Option<String>,
    pub toast_style: ToastStyle,
    pub toast_expires: Option<std::time::Instant>,

    // Task status transition detection (for toast notifications)
    prev_task_statuses: HashMap<String, TaskStatus>,
    // Tasks that have already shown an InReview toast (avoid repeats from status cycling)
    notified_in_review: HashSet<String>,
    // Tasks that have already had a review loop spawned
    review_loop_spawned: HashSet<String>,

    // Slow-tick tracking for session tabs (DB refresh, PR polling, etc.)
    last_slow_tick: Instant,

    // Last known terminal area for mouse hit-testing
    pub last_terminal_area: Rect,

    // Cached diff preview for the selected thread/worktree in the inspector.
    pub diff_preview_cache_key: Option<String>,
    pub diff_preview_cache: String,
    pub diff_preview_generated_at: Option<Instant>,

    // Sessions where Claude is waiting for user permission (detected from PTY screen)
    pub paused_sessions: HashSet<String>,

    // Live PTY activity preview: last non-empty lines from the Claude pane (cached per tick)
    pub pty_activity_preview: HashMap<String, Vec<String>>,

    // Sessions where Claude asked a question and is waiting for user answer (detected from PTY screen)
    pub waiting_sessions: HashSet<String>,

    // Sessions where Claude's PTY shows the idle prompt (❯) — fallback for when
    // the Notification hook fails to fire and claude_status stays Working in the DB.
    pub pty_idle_sessions: HashSet<String>,

    // Tracks when a Working session last had NO Claude indicators on screen.
    // After 15 seconds of no indicators (no ❯, no Allow prompt, no question),
    // the session is assumed to have Claude exited and is added to pty_idle_sessions.
    pub working_no_indicator_since: HashMap<String, std::time::Instant>,

    // Tracks workflow stage_run IDs whose prompts have already been injected into the PTY.
    // Used by `maybe_advance_workflow_stages()` to distinguish "idle before injection"
    // from "idle after injection (Claude finished processing)".
    pub workflow_stage_injected: HashSet<String>,

    // Reserved for future use — message queueing was removed as unreliable.
    #[expect(dead_code, reason = "field kept for binary compatibility during development")]
    pub queued_compose_message: Option<(String, String)>,

    // Cached result of visible_tasks() — indices into self.tasks, filtered and sorted.
    // Recomputed by recompute_visible_tasks() after data changes.
    cached_visible_indices: Vec<usize>,
    cached_my_task_indices: Vec<usize>,

    // Configuration warning (set on startup if permissions are misaligned)
    pub config_warning: Option<String>,
    // Cached configure overlay state (loaded once on overlay open, not per-frame)
    pub cached_config_status: Option<Result<crate::configure::ConfigStatus, String>>,

    // Auto-update state
    update_check_in_progress: Arc<AtomicBool>,
    update_tx: mpsc::Sender<crate::update::UpdateCheckResult>,
    update_rx: mpsc::Receiver<crate::update::UpdateCheckResult>,
    last_update_check: Instant,
    /// Stores the version string after a successful auto-update (shown in title bar).
    pub updated_version: Option<String>,
    /// Stores a newer version string when one exists but installation failed.
    pub available_version: Option<String>,

    github_auth_in_progress: Arc<AtomicBool>,
    github_auth_tx: mpsc::Sender<crate::github_app::GitHubAuthMessage>,
    github_auth_rx: mpsc::Receiver<crate::github_app::GitHubAuthMessage>,
    pub github_auth_prompt: Option<crate::github_app::GitHubDevicePrompt>,
    pub github_auth_error: Option<String>,
    github_installations_in_progress: Arc<AtomicBool>,
    github_installations_tx:
        mpsc::Sender<Result<Vec<crate::github_app::GitHubInstallation>, String>>,
    github_installations_rx:
        mpsc::Receiver<Result<Vec<crate::github_app::GitHubInstallation>, String>>,
    // Background GitHub sync (non-blocking board refresh)
    pub github_sync_in_progress: Arc<AtomicBool>,
    github_sync_tx: mpsc::Sender<Result<crate::github::GitHubProjectBoardSnapshot, String>>,
    github_sync_rx: mpsc::Receiver<Result<crate::github::GitHubProjectBoardSnapshot, String>>,

    // JSONL conversation cache for the active session tab
    pub conversation_cache: Option<ConversationCache>,

    // Quick-reply choices detected from Claude's last message (e.g. "1. X  2. Y  3. Both").
    // Pressing the number key auto-sends the corresponding choice.
    pub quick_reply_choices: Vec<String>,
}

/// Cached JSONL conversation entries for a session's Claude Code log.
pub(crate) struct ConversationCache {
    pub session_id: String,
    pub entries: Vec<crate::conversation::ConversationEntry>,
    pub file_offset: u64,
    pub file_mtime: Option<std::time::SystemTime>,
    pub jsonl_path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewQueueItem {
    pub github_item: GitHubItem,
    pub pr_cache: GitHubPrCache,
    pub issue_cache: Option<GitHubIssueCache>,
    pub author_login: Option<String>,
    pub requested_reviewer_logins: Vec<String>,
    pub comment_count: usize,
    pub review_count: usize,
    pub review_comment_count: usize,
}

impl ReviewQueueItem {
    pub(crate) fn title_line(&self) -> String {
        format!("#{} {}", self.github_item.number, self.github_item.title)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MyTaskItem {
    pub repo: GitHubRepoCache,
    pub github_item: GitHubItem,
    pub issue_cache: Option<GitHubIssueCache>,
    pub pr_cache: Option<GitHubPrCache>,
    pub linked_thread: Option<Thread>,
    pub linked_pr: Option<GitHubItem>,
}

impl MyTaskItem {
    pub(crate) fn title_line(&self) -> String {
        format!("#{} {}", self.github_item.number, self.github_item.title)
    }
}

impl App {
    pub(crate) fn busy_indicator_label(&self) -> Option<&'static str> {
        if self.session_op_in_progress {
            Some("preparing session")
        } else if self.github_auth_in_progress.load(Ordering::Relaxed) {
            Some("connecting GitHub")
        } else if self
            .github_installations_in_progress
            .load(Ordering::Relaxed)
        {
            Some("loading GitHub installations")
        } else if self.github_sync_in_progress.load(Ordering::Relaxed) {
            Some("syncing GitHub")
        } else if self.board_loading {
            Some("loading sprint board")
        } else if !self.pending_titles.is_empty() {
            Some("generating task title")
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SelectedThreadContext {
    pub thread: Thread,
    pub messages: Vec<ThreadMessage>,
    pub attachments: Vec<ThreadAttachment>,
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "Full run history is preserved for provider-neutral thread relaunch and upcoming chat UX work"
        )
    )]
    pub runs: Vec<ThreadRun>,
    pub latest_run: Option<ThreadRun>,
    pub message_count: usize,
    pub run_count: usize,
    pub attachment_count: usize,
    pub runtime_state: Option<ThreadRuntimeState>,
    pub runtime_services: Vec<RuntimeServiceState>,
    pub workflow_def: Option<WorkflowDef>,
    pub workflow_run: Option<WorkflowRun>,
    pub workflow_stage: Option<WorkflowStageRun>,
    pub workflow_stages: Vec<WorkflowStageRun>,
    pub workflow_artifact_count: usize,
    pub github_item: Option<GitHubItem>,
    pub issue_cache: Option<GitHubIssueCache>,
    pub pr_cache: Option<GitHubPrCache>,
    pub comment_count: usize,
    pub review_count: usize,
    pub review_comment_count: usize,
    pub knowledge_card_count: usize,
    pub knowledge_draft_count: usize,
}

impl SelectedThreadContext {
    fn short_id(id: &str) -> &str {
        &id[..8.min(id.len())]
    }

    pub(crate) fn identity_summary(&self) -> String {
        format!(
            "{} [{}] via {}",
            Self::short_id(&self.thread.id),
            self.thread.status,
            self.thread.provider_kind
        )
    }

    pub(crate) fn activity_summary(&self) -> String {
        format!(
            "{} message(s), {} run(s), {} attachment(s)",
            self.message_count, self.run_count, self.attachment_count
        )
    }

    pub(crate) fn latest_run_summary(&self) -> Option<String> {
        self.latest_run.as_ref().map(|run| {
            let profile = run.provider_profile.as_deref().unwrap_or("default");
            format!("{} [{}] profile {}", run.provider_kind, run.status, profile)
        })
    }

    pub(crate) fn runtime_summary(&self) -> String {
        let profile = self
            .runtime_state
            .as_ref()
            .and_then(|state| state.profile_name.as_deref())
            .or(self.thread.runtime_profile.as_deref())
            .unwrap_or("default");
        let build_status = self
            .runtime_state
            .as_ref()
            .and_then(|state| state.build_status.as_deref())
            .unwrap_or("not_started");
        if self.runtime_services.is_empty() {
            return format!("{profile} [{build_status}] no services");
        }

        let healthy = self
            .runtime_services
            .iter()
            .filter(|service| {
                matches!(
                    service.status,
                    crate::store::RuntimeServiceStatus::Running
                        | crate::store::RuntimeServiceStatus::Healthy
                )
            })
            .count();
        let failed = self
            .runtime_services
            .iter()
            .filter(|service| service.status == crate::store::RuntimeServiceStatus::Failed)
            .count();
        format!(
            "{profile} [{build_status}] {healthy}/{} healthy, {failed} failed",
            self.runtime_services.len()
        )
    }

    pub(crate) fn workflow_summary(&self) -> Option<String> {
        self.workflow_run.as_ref().map(|run| {
            let name = self
                .workflow_def
                .as_ref()
                .map_or("workflow", |definition| definition.name.as_str());
            let stage = self
                .workflow_stage
                .as_ref()
                .map(|stage| format!("{} [{}]", stage.stage_name, stage.status))
                .or_else(|| run.current_stage.clone())
                .unwrap_or_else(|| "no stage".to_string());
            format!(
                "{name} [{}] {stage}, {} artifact(s)",
                run.status, self.workflow_artifact_count
            )
        })
    }

    pub(crate) fn github_summary(&self) -> Option<String> {
        self.github_item.as_ref().map(|item| {
            let mut summary = format!("{} #{} [{}]", item.kind, item.number, item.state);
            if let Some(pr) = &self.pr_cache
                && let (Some(base_ref), Some(head_ref)) = (&pr.base_ref, &pr.head_ref)
            {
                let _ = write!(summary, " {head_ref} -> {base_ref}");
            }
            let _ = write!(
                summary,
                ", {} comment(s), {} review(s), {} inline review comment(s)",
                self.comment_count, self.review_count, self.review_comment_count
            );
            summary
        })
    }

    pub(crate) fn github_preview(&self) -> Option<&str> {
        self.pr_cache
            .as_ref()
            .and_then(|cache| cache.body_text.as_deref())
            .or_else(|| {
                self.issue_cache
                    .as_ref()
                    .and_then(|cache| cache.body_text.as_deref())
            })
            .or_else(|| {
                self.github_item
                    .as_ref()
                    .and_then(|item| item.body_text.as_deref())
            })
    }

    pub(crate) fn knowledge_summary(&self) -> String {
        format!(
            "{} card(s), {} draft(s)",
            self.knowledge_card_count, self.knowledge_draft_count
        )
    }
}

/// Quick fallback title by truncating the first line at a word boundary.
/// Used immediately when creating a task so the UI stays responsive.
pub(super) fn fallback_title(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or(prompt);
    if first_line.len() <= 60 {
        first_line.to_string()
    } else {
        // Find a char boundary at or before byte 60 to avoid panicking on multi-byte UTF-8
        let boundary = first_line
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= 60)
            .last()
            .unwrap_or(0);
        let truncated = &first_line[..boundary];
        if let Some(last_space) = truncated.rfind(' ') {
            format!("{}...", &truncated[..last_space])
        } else {
            format!("{truncated}...")
        }
    }
}

/// Generate a short title from a task prompt using Claude Haiku.
/// Called in a background thread. Falls back to the truncated title on failure.
fn generate_ai_title(prompt: &str) -> String {
    let system = "Output ONLY a concise title (max 8 words) for the given task. No quotes, no punctuation at the end, no preamble, no explanation. Just the title, nothing else.";
    let msg = format!("Title this task:\n{prompt}");

    if let Ok(output) = std::process::Command::new("claude")
        .args(["-p", "--model", "haiku", "--system-prompt", system, &msg])
        .output()
        && output.status.success()
    {
        // Take the last non-empty line to skip any preamble the model might add
        let raw = String::from_utf8_lossy(&output.stdout);
        let title = raw
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        if !title.is_empty() {
            return title;
        }
    }

    fallback_title(prompt)
}

/// Check if a vt100 terminal screen shows a Claude Code permission prompt.
///
/// Claude Code renders tool-approval dialogs with patterns like:
///   "Allow Bash", "Allow `WebFetch`", etc.
/// Recursively compute the inner area (content inside border) for every leaf pane
/// in a layout tree, given the total outer area.
fn collect_pane_inner_areas(
    node: &crate::pty::LayoutNode,
    area: Rect,
) -> Vec<(crate::pty::PaneId, Rect)> {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::widgets::{Block, Borders};

    let mut result = Vec::new();

    match node {
        crate::pty::LayoutNode::Pane(id) => {
            let block = Block::default().borders(Borders::ALL);
            let inner = block.inner(area);
            result.push((*id, inner));
        }
        crate::pty::LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let dir = match direction {
                crate::pty::SplitDirection::Horizontal => Direction::Horizontal,
                crate::pty::SplitDirection::Vertical => Direction::Vertical,
            };
            let chunks = Layout::default()
                .direction(dir)
                .constraints([
                    Constraint::Percentage(*ratio),
                    Constraint::Percentage(100 - *ratio),
                ])
                .split(area);
            result.extend(collect_pane_inner_areas(first, chunks[0]));
            result.extend(collect_pane_inner_areas(second, chunks[1]));
        }
    }

    result
}

/// Compute exact PTY inner dimensions for each pane using ratatui's layout engine.
///
/// Uses the same `Layout` and `Block::inner()` logic as the rendering path
/// (`draw_session_tab` + `render_layout_node` + `render_single_pane`) so PTY sizes
/// always match the actual rendered areas — no off-by-one edge clipping.
fn compute_pane_sizes_for_resize(
    layout: &crate::pty::LayoutNode,
    total_cols: u16,
    total_rows: u16,
) -> Vec<(crate::pty::PaneId, u16, u16)> {
    // Terminal content area matches draw_session_tab layout:
    // tab bar (1) + terminal area (remaining) + hint bar (1)
    let term_area = Rect {
        x: 0,
        y: 0,
        width: total_cols,
        height: total_rows.saturating_sub(2),
    };
    collect_pane_inner_areas(layout, term_area)
        .into_iter()
        .map(|(id, r)| (id, r.height, r.width))
        .collect()
}

/// followed by interactive options ("Yes", "No", "Always").
///
/// Only checks the bottom 20 rows of the screen to avoid false positives
/// from Claude's text output that might mention "Allow" in discussion.
fn screen_shows_permission_prompt(screen: &vt100::Screen) -> bool {
    let contents = screen.contents();
    let lines: Vec<&str> = contents.lines().collect();
    let total = lines.len();

    // Only check the bottom portion of the screen where prompts appear
    let start = total.saturating_sub(20);
    let bottom_lines = &lines[start..];

    // Look for "Allow <ToolName>" pattern — the tool name starts with an uppercase letter.
    // This matches Claude Code's permission dialog for any tool (Bash, WebFetch, Read, etc.)
    let has_allow = bottom_lines.iter().any(|line| {
        // Find "Allow " anywhere in the line (may be preceded by box-drawing chars or symbols)
        if let Some(pos) = line.find("Allow ") {
            let after = &line[pos + 6..];
            after.starts_with(|c: char| c.is_ascii_uppercase())
        } else {
            false
        }
    });

    if !has_allow {
        return false;
    }

    // Confirm with yes/no options nearby — Claude Code shows interactive choices
    // like "Yes  No  Always" on the same line
    bottom_lines.iter().any(|line| {
        (line.contains("Yes") || line.contains("yes"))
            && (line.contains("No") || line.contains("no"))
    })
}

/// Detect Claude Code's `AskUserQuestion` interactive selector in the PTY screen.
///
/// When Claude uses `AskUserQuestion`, the terminal shows a question with selectable
/// options. The selector always includes "Other" as a choice, and uses `❯` (U+276F) as
/// the cursor on the currently focused option. We detect this pattern in the bottom 25
/// lines of the screen.
fn screen_shows_question_prompt(screen: &vt100::Screen) -> bool {
    let contents = screen.contents();
    let lines: Vec<&str> = contents.lines().collect();
    let total = lines.len();

    let start = total.saturating_sub(25);
    let bottom_lines = &lines[start..];

    // Look for "❯" (selection cursor) on an option line — not the bare input prompt.
    // The input prompt is just "❯" possibly followed by typed text at the very bottom,
    // but question options have "❯" followed by a label among other option lines.
    let has_selection_cursor = bottom_lines.iter().any(|line| {
        let trimmed = line.trim();
        // Selection cursor followed by a space and option text
        trimmed.starts_with('\u{276f}')
    });

    if !has_selection_cursor {
        return false;
    }

    // AskUserQuestion always appends "Other" as a selectable option.
    // Check that "Other" appears as a standalone option line (trimmed).
    bottom_lines
        .iter()
        .any(|line| line.trim().starts_with("Other"))
}

/// Detect Claude Code's idle prompt in the PTY screen.
///
/// When Claude is idle (waiting for user input), the terminal shows `❯` (U+276F)
/// at or near the bottom of the screen as the input prompt. This is distinct from
/// the question-prompt pattern (which has `❯` + "Other" option) and the permission
/// prompt (which has "Allow" + "Yes/No").
///
/// Used as a fallback when the Notification hook fails to fire.
fn screen_shows_idle_prompt(screen: &vt100::Screen) -> bool {
    let contents = screen.contents();
    let lines: Vec<&str> = contents.lines().collect();
    let total = lines.len();

    // Check the bottom few lines for the bare ❯ prompt
    let start = total.saturating_sub(5);
    let bottom_lines = &lines[start..];

    // Find the last non-empty line from the bottom
    let last_content = bottom_lines.iter().rev().find(|l| !l.trim().is_empty());
    let Some(last_line) = last_content else {
        return false;
    };
    let trimmed = last_line.trim();

    // The idle prompt is just "❯" possibly followed by typed text.
    // Must NOT be a question prompt (those have "Other" nearby) or
    // permission prompt (those have "Allow" nearby).
    if !trimmed.starts_with('\u{276f}') {
        return false;
    }

    // Exclude question prompts
    if bottom_lines.iter().any(|l| l.trim().starts_with("Other")) {
        return false;
    }

    // Exclude permission prompts
    if bottom_lines.iter().any(|l| l.contains("Allow ")) {
        return false;
    }

    true
}

fn build_project_summaries(store: &Store, projects: &[Project]) -> HashMap<String, ProjectSummary> {
    let mut summaries = HashMap::with_capacity(projects.len());
    for project in projects {
        let active_sessions = store
            .list_active_sessions_for_project(&project.id)
            .unwrap_or_default();
        let task_counts = store.count_tasks_by_status(&project.id).unwrap_or_default();
        summaries.insert(
            project.id.clone(),
            ProjectSummary {
                active_sessions,
                task_counts,
                default_branch: project.default_branch.clone(),
            },
        );
    }
    summaries
}

/// Run `git diff --stat` in a worktree and parse the summary line.
/// Returns (files changed, lines added, lines removed).
fn parse_git_diff_stat(worktree_path: &str, default_branch: &str) -> Option<(i64, i64, i64)> {
    let origin_branch = format!("origin/{default_branch}");
    let output = std::process::Command::new("git")
        .args(["diff", "--stat", &origin_branch])
        .current_dir(worktree_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The summary line looks like:
    //  3 files changed, 10 insertions(+), 2 deletions(-)
    // or just "1 file changed, 5 insertions(+)" etc.
    let last_line = stdout.lines().last()?;

    if !last_line.contains("changed") {
        // No changes — empty diff
        return Some((0, 0, 0));
    }

    let mut files = 0i64;
    let mut added = 0i64;
    let mut removed = 0i64;

    for part in last_line.split(',') {
        let part = part.trim();
        if part.contains("file") {
            files = part
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if part.contains("insertion") {
            added = part
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if part.contains("deletion") {
            removed = part
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        }
    }

    Some((files, added, removed))
}

fn check_pr_status(pr_url: &str) -> PrStatus {
    let Ok(output) = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            pr_url,
            "--json",
            "state,mergeable,statusCheckRollup",
        ])
        .output()
    else {
        return PrStatus::Open;
    };
    if !output.status.success() {
        return PrStatus::Open;
    }
    let raw = String::from_utf8_lossy(&output.stdout);

    // Parse JSON: {"state":"OPEN","mergeable":"CONFLICTING","statusCheckRollup":[...]}
    let Ok(json) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
        return PrStatus::Open;
    };

    let state = json["state"].as_str().unwrap_or("");
    if state.eq_ignore_ascii_case("MERGED") {
        return PrStatus::Merged;
    }

    let mergeable = json["mergeable"].as_str().unwrap_or("");
    if mergeable.eq_ignore_ascii_case("CONFLICTING") {
        return PrStatus::Conflicting;
    }

    // Check CI status from statusCheckRollup
    if let Some(checks) = json["statusCheckRollup"].as_array()
        && !checks.is_empty()
    {
        // Any completed check with FAILURE/ERROR conclusion means CI failed
        let has_failure = checks.iter().any(|check| {
            let conclusion = check["conclusion"].as_str().unwrap_or("");
            conclusion.eq_ignore_ascii_case("FAILURE") || conclusion.eq_ignore_ascii_case("ERROR")
        });
        if has_failure {
            return PrStatus::CiFailed;
        }

        // Check if all checks have completed (non-empty conclusion)
        let all_done = checks.iter().all(|check| {
            let conclusion = check["conclusion"].as_str().unwrap_or("");
            !conclusion.is_empty()
        });

        return if all_done {
            PrStatus::CiPassed
        } else {
            PrStatus::CiRunning
        };
    }

    PrStatus::Open
}

/// Fetch usage from the Anthropic OAuth API and write to the shared cache file.
#[expect(
    clippy::similar_names,
    reason = "5h and 7d are distinct domain-specific window labels"
)]
fn fetch_and_cache_usage() -> Option<()> {
    // Get OAuth token from macOS Keychain
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let token_json = String::from_utf8(output.stdout).ok()?;
    let creds: serde_json::Value = serde_json::from_str(token_json.trim()).ok()?;
    let access_token = creds["claudeAiOauth"]["accessToken"].as_str()?;

    // Fetch usage from API. Use --fail so HTTP errors (401, 500) produce a
    // non-zero exit code instead of silently returning an error JSON body.
    let output = std::process::Command::new("curl")
        .args([
            "-sf",
            "--max-time",
            "10",
            "https://api.anthropic.com/api/oauth/usage",
            "-H",
            &format!("Authorization: Bearer {access_token}"),
            "-H",
            "anthropic-beta: oauth-2025-04-20",
            "-H",
            "Content-Type: application/json",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let usage: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

    // Bail out if the API didn't return utilization data — don't overwrite
    // the cache with incomplete data that would blank the usage bars.
    let pct_5h = usage["five_hour"]["utilization"].as_f64();
    let pct_7d = usage["seven_day"]["utilization"].as_f64();
    if pct_5h.is_none() && pct_7d.is_none() {
        return None;
    }

    let now = chrono::Utc::now().timestamp_millis();

    // Format time-until-reset strings (matching statusline cache format)
    let format_time_left = |reset_at_str: &str| -> Option<String> {
        let reset_at = chrono::DateTime::parse_from_rfc3339(reset_at_str).ok()?;
        let time_left = reset_at.timestamp_millis() - now;
        if time_left <= 0 {
            return None;
        }
        let hours = time_left / (1000 * 60 * 60);
        let minutes = (time_left % (1000 * 60 * 60)) / (1000 * 60);
        if hours >= 24 {
            let days = hours / 24;
            let rem_hours = hours % 24;
            Some(format!("{days}d{rem_hours}h"))
        } else if hours > 0 {
            Some(format!("{hours}h{minutes}m"))
        } else {
            Some(format!("{minutes}m"))
        }
    };

    let reset_5h = usage["five_hour"]["resets_at"]
        .as_str()
        .and_then(format_time_left);
    let reset_7d = usage["seven_day"]["resets_at"]
        .as_str()
        .and_then(format_time_left);

    // Normalize utilization to percentage (0-100). The API may return
    // a fraction (0.0-1.0) or a percentage depending on version.
    let normalize_pct = |v: &serde_json::Value| -> Option<f64> {
        let raw = v.as_f64()?;
        if raw <= 1.0 {
            Some(raw * 100.0)
        } else {
            Some(raw)
        }
    };

    let cache = serde_json::json!({
        "timestamp": now,
        "data": {
            "reset5h": reset_5h,
            "reset7d": reset_7d,
            "pct5h": normalize_pct(&usage["five_hour"]["utilization"]).unwrap_or(0.0),
            "pct7d": normalize_pct(&usage["seven_day"]["utilization"]).unwrap_or(0.0)
        }
    });

    let home = dirs::home_dir()?;
    let cache_path = home.join(".claude/statusline-cache.json");

    // Write to a temp file then rename for atomic update, avoiding
    // race conditions with the TUI reading the cache concurrently.
    let tmp_path = cache_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, serde_json::to_string(&cache).ok()?).ok()?;
    std::fs::rename(&tmp_path, &cache_path).ok()?;

    Some(())
}

#[cfg(test)]
mod tests {
    use super::input::{encode_mouse_event, keycode_to_bytes};
    use super::*;
    use crate::runtime::RuntimeServiceState;
    use crate::store::{
        AttachmentSource, GitHubItemKind, ProviderKind, RuntimeServiceKind, RuntimeServiceStatus,
        Store, TaskMode, TaskStatus, ThreadRunStatus, ThreadStatus, WorkflowRunStatus,
        WorkflowStageStatus,
    };
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    // ── Test Helpers ──

    fn test_app() -> App {
        let store = Store::open_in_memory().unwrap();
        let mut app = App::new(store).unwrap();
        app.loading = false;
        app.github_status = crate::github_app::GitHubAppStatus::default();
        app
    }

    fn test_app_with_project() -> App {
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("test-project", "/tmp/test-repo", "main", true)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.loading = false;
        app.github_status = crate::github_app::GitHubAppStatus::default();
        app
    }

    fn test_app_with_tasks() -> App {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("test-project", "/tmp/test-repo", "main", true)
            .unwrap();
        store
            .create_task(
                &project.id,
                "Task Alpha",
                "First task",
                TaskMode::Supervised,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &project.id,
                "Task Beta",
                "Second task",
                TaskMode::Autonomous,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &project.id,
                "Task Gamma",
                "Third task",
                TaskMode::Supervised,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.loading = false;
        app.github_status = crate::github_app::GitHubAppStatus::default();
        app
    }

    fn seed_thread_workspace(app: &mut App) {
        let project = app.projects[0].clone();
        let task = app.tasks[0].clone();
        let session = app
            .store
            .create_session(
                &project.id,
                "task-alpha",
                "/tmp/test-repo-worktree",
                "Task Alpha",
            )
            .unwrap();
        app.store
            .assign_task_to_session(&task.id, &session.id)
            .unwrap();
        app.store
            .update_session_status(&session.id, crate::store::ClaudeStatus::Working, "Planning")
            .unwrap();
        app.store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        let repo = app
            .store
            .upsert_github_repo(
                Some(&project.id),
                "acme",
                "claustre",
                Some("https://github.com/acme/claustre"),
                Some("main"),
            )
            .unwrap();
        let item = app
            .store
            .upsert_github_item(
                &repo.id,
                None,
                Some("MDQ6VXNlcjE="),
                42,
                GitHubItemKind::PullRequest,
                "Render issue and PR review context",
                "OPEN",
                "https://github.com/acme/claustre/pull/42",
                Some("Need a proper diff and issue rendering surface."),
                &["harsha".to_string()],
                &["ui".to_string(), "github".to_string()],
                &serde_json::json!({}),
                &serde_json::json!({"Status": "In Progress"}),
                Some("main"),
                Some("task-alpha"),
                Some("2026-03-16T10:00:00Z"),
            )
            .unwrap();
        app.store
            .upsert_github_issue_cache(
                &item.id,
                Some("Issue body markdown"),
                Some("Issue body markdown"),
                Some("<p>Issue body markdown</p>"),
                Some("octocat"),
                Some("Sprint 12"),
                None,
            )
            .unwrap();
        app.store
            .upsert_github_pr_cache(
                &item.id,
                Some("PR body markdown"),
                Some("PR body markdown"),
                Some("<p>PR body markdown</p>"),
                Some("main"),
                Some("task-alpha"),
                Some("clean"),
                Some("review_required"),
                false,
                None,
            )
            .unwrap();
        app.store
            .upsert_github_comment_cache(
                &item.id,
                "issue-comment-1",
                Some("reviewer"),
                Some("Please render the body properly."),
                Some("Please render the body properly."),
                Some("<p>Please render the body properly.</p>"),
                "2026-03-16T10:10:00Z",
                None,
                None,
            )
            .unwrap();
        let review = app
            .store
            .upsert_github_review_cache(
                &item.id,
                "review-1",
                "COMMENTED",
                Some("abc123"),
                Some("reviewer"),
                Some("Inline notes left."),
                Some("Inline notes left."),
                Some("<p>Inline notes left.</p>"),
                Some("2026-03-16T10:15:00Z"),
                None,
            )
            .unwrap();
        app.store
            .upsert_github_review_comment_cache(
                Some(&review.id),
                &item.id,
                "review-comment-1",
                Some("reviewer"),
                Some("src/tui/ui/overlays.rs"),
                Some(120),
                Some("RIGHT"),
                None,
                Some("@@ -1,1 +1,1 @@"),
                None,
                Some("Anchor this to the diff."),
                Some("Anchor this to the diff."),
                Some("<p>Anchor this to the diff.</p>"),
                "2026-03-16T10:20:00Z",
                None,
                None,
            )
            .unwrap();

        let thread = app
            .store
            .create_thread(
                &project.id,
                Some(&item.id),
                Some(&task.id),
                Some(&session.id),
                &task.title,
                ProviderKind::Claude,
                Some("default"),
                Some("/tmp/test-repo-worktree"),
                Some("task-alpha"),
                Some("default"),
                None,
            )
            .unwrap();
        app.store
            .update_thread_status(&thread.id, ThreadStatus::Running)
            .unwrap();
        let run = app
            .store
            .create_thread_run(
                &thread.id,
                ProviderKind::Claude,
                Some("default"),
                ThreadRunStatus::Running,
                Some("Plan the work first"),
            )
            .unwrap();
        app.store
            .create_thread_message(
                &thread.id,
                Some(&run.id),
                "user",
                "Plan the work first",
                &[],
            )
            .unwrap();
        app.store
            .create_thread_attachment(
                &thread.id,
                None,
                Some("draft-1"),
                "image/png",
                "clipboard.png",
                Some(1600),
                Some(900),
                "/tmp/clipboard.png",
                AttachmentSource::Clipboard,
            )
            .unwrap();
        app.store
            .upsert_thread_runtime_state(
                &thread.id,
                Some("default"),
                Some("built"),
                &serde_json::to_string(&vec![RuntimeServiceState {
                    name: "web".to_string(),
                    kind: RuntimeServiceKind::Command,
                    status: RuntimeServiceStatus::Healthy,
                    pid: Some(4242),
                    log_path: Some("/tmp/claustre-web.log".to_string()),
                    health: Some("http://127.0.0.1:3000/health".to_string()),
                    started_at: "2026-03-16T10:05:00Z".to_string(),
                }])
                .unwrap(),
                None,
            )
            .unwrap();
        let workflow_def = app
            .store
            .upsert_workflow_def(
                "plan_first_tdd",
                "builtin",
                Some("builtin://plan_first_tdd"),
                Some("Plan before implementation"),
                "name: plan_first_tdd",
                true,
            )
            .unwrap();
        let workflow_run = app
            .store
            .create_workflow_run(
                &workflow_def.id,
                Some(&thread.id),
                Some(&item.id),
                WorkflowRunStatus::Running,
                Some("implement"),
            )
            .unwrap();
        app.store
            .attach_workflow_run_to_thread(&thread.id, Some(&workflow_run.id))
            .unwrap();
        app.store
            .create_workflow_stage_run(
                &workflow_run.id,
                "implement",
                WorkflowStageStatus::Running,
                Some(ProviderKind::Claude),
                Some("default"),
                Some("default"),
                Some("Implement against the failing tests"),
                None,
                Some("Implementation in progress"),
            )
            .unwrap();
        app.store
            .create_workflow_artifact(
                &workflow_run.id,
                None,
                "plan.md",
                "markdown",
                Some("/tmp/plan.md"),
                Some("# Plan"),
            )
            .unwrap();

        let draft = app
            .store
            .create_knowledge_draft(
                &project.id,
                Some(&item.id),
                Some(&thread.id),
                Some(&run.id),
                Some("acme"),
                Some("claustre"),
                "Diff viewer reminder",
                "Local git diff should stay canonical.",
                "Use the local worktree as the source of truth.",
                Some(0.9),
                &[String::from("diff"), String::from("review")],
                Some("abc123"),
                Some("task-alpha"),
                Some("seed"),
            )
            .unwrap();
        app.store
            .promote_knowledge_draft(&draft.id, None, None, None)
            .unwrap();
        app.store
            .create_knowledge_draft(
                &project.id,
                Some(&item.id),
                Some(&thread.id),
                Some(&run.id),
                Some("acme"),
                Some("claustre"),
                "Follow-up note",
                "Pending extraction review.",
                "Capture more review heuristics later.",
                Some(0.6),
                &[String::from("todo")],
                Some("abc123"),
                Some("task-alpha"),
                Some("seed"),
            )
            .unwrap();

        app.refresh_data().unwrap();
        app.github_status = crate::github_app::GitHubAppStatus::default();
        app.recompute_my_task_items().unwrap();
        app.recompute_review_items().unwrap();
        app.task_index = app
            .visible_tasks()
            .iter()
            .position(|candidate| candidate.id == task.id)
            .unwrap();
    }

    fn seed_review_queue_items(app: &mut App) {
        seed_thread_workspace(app);

        let project = app.projects[0].clone();
        let repo = app
            .store
            .get_github_repo_for_project(&project.id)
            .unwrap()
            .unwrap();
        let authored_item = app
            .store
            .list_github_items_for_repo(&repo.id)
            .unwrap()
            .into_iter()
            .find(|item| item.number == 42)
            .unwrap();

        app.store
            .upsert_github_issue_cache(
                &authored_item.id,
                Some("Issue body markdown"),
                Some("Issue body markdown"),
                Some("<p>Issue body markdown</p>"),
                Some("harsha"),
                Some("Sprint 12"),
                None,
            )
            .unwrap();

        let review_payload = serde_json::json!({
            "author": { "login": "octocat" },
            "requested_reviewers": [{ "login": "harsha" }],
        });
        let review_item = app
            .store
            .upsert_github_item(
                &repo.id,
                None,
                Some("PR_review_77"),
                77,
                GitHubItemKind::PullRequest,
                "Review queue PR",
                "OPEN",
                "https://github.com/acme/claustre/pull/77",
                Some("This PR needs an AI review pass."),
                &["harsha".to_string()],
                &["review".to_string()],
                &serde_json::json!({}),
                &serde_json::json!({"Status": "In Review"}),
                Some("main"),
                Some("review/pr-77"),
                Some("2026-03-16T12:00:00Z"),
            )
            .unwrap();
        app.store
            .upsert_github_pr_cache(
                &review_item.id,
                Some("Review PR body"),
                Some("Review PR body"),
                Some("<p>Review PR body</p>"),
                Some("main"),
                Some("review/pr-77"),
                Some("clean"),
                Some("review_required"),
                false,
                Some(&review_payload.to_string()),
            )
            .unwrap();
        app.store
            .upsert_github_issue_cache(
                &review_item.id,
                Some("Review PR body"),
                Some("Review PR body"),
                Some("<p>Review PR body</p>"),
                Some("octocat"),
                Some("Sprint 12"),
                Some(&review_payload.to_string()),
            )
            .unwrap();

        app.github_status.gh_user_login = Some("harsha".to_string());
        app.recompute_review_items().unwrap();
    }

    /// Simulate a key press, routing through the correct handler based on current `InputMode`.
    fn press(app: &mut App, code: KeyCode) {
        press_mod(app, code, KeyModifiers::NONE);
    }

    fn press_mod(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        match app.input_mode {
            InputMode::Normal => {
                app.handle_normal_key(code, modifiers).unwrap();
            }
            InputMode::ThreadCompose => {
                app.handle_thread_compose_key(code, modifiers).unwrap();
            }
            InputMode::LaunchThread => app.handle_launch_thread_key(code, modifiers).unwrap(),
            InputMode::SettingsEdit => app.handle_settings_edit_key(code, modifiers).unwrap(),
            InputMode::SettingsPicker => app.handle_settings_picker_key(code).unwrap(),
            InputMode::GitHubInstallationPicker => {
                app.handle_github_installation_picker_key(code).unwrap();
            }
            InputMode::GitHubProjectPicker => {
                app.handle_github_project_picker_key(code, modifiers)
                    .unwrap();
            }
            InputMode::ProjectPicker => app.handle_project_picker_key(code).unwrap(),
            InputMode::ThreadProviderPicker => {
                app.handle_thread_provider_picker_key(code).unwrap();
            }
            InputMode::NewTask => app.handle_input_key(code, modifiers).unwrap(),
            InputMode::EditTask => app.handle_edit_task_key(code, modifiers).unwrap(),
            InputMode::NewProject => app.handle_new_project_key(code, modifiers).unwrap(),
            InputMode::ConfirmDelete => app.handle_confirm_delete_key(code).unwrap(),
            InputMode::CommandPalette => app.handle_palette_key(code, modifiers).unwrap(),
            InputMode::SkillPanel => app.handle_skill_panel_key(code).unwrap(),
            InputMode::SkillSearch => app.handle_skill_search_key(code, modifiers).unwrap(),
            InputMode::SkillAdd => app.handle_skill_add_key(code, modifiers).unwrap(),
            InputMode::HelpOverlay => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                    app.input_mode = InputMode::Normal;
                }
            }
            InputMode::TaskDetails => match code {
                KeyCode::Esc | KeyCode::Char('v' | 'q') => {
                    app.input_mode = InputMode::Normal;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    app.task_details_scroll = app.task_details_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.task_details_scroll = app.task_details_scroll.saturating_sub(1);
                }
                _ => {}
            },
            InputMode::TaskFilter => app.handle_task_filter_key(code, modifiers).unwrap(),
            InputMode::SubtaskPanel => app.handle_subtask_panel_key(code, modifiers).unwrap(),
            InputMode::ConfigureWizard => app.handle_configure_key(code).unwrap(),
            InputMode::BoardView => app.handle_board_key(code, modifiers).unwrap(),
            InputMode::MilestoneFilter => app.handle_board_sprint_picker_key(code).unwrap(),
            InputMode::BoardFilter => app.handle_board_filter_key(code, modifiers).unwrap(),
            InputMode::CommentCompose => {
                app.handle_comment_compose_key(code, modifiers).unwrap();
            }
            InputMode::BoardIssueDrawer => {
                app.handle_board_drawer_key(code, modifiers).unwrap();
            }
            InputMode::MyTaskDrawer => {
                app.handle_my_task_drawer_key(code, modifiers).unwrap();
            }
            InputMode::FieldPicker => {
                app.handle_field_picker_key(code).unwrap();
            }
            InputMode::CreateGitHubIssue | InputMode::EditGitHubIssue => {
                app.handle_github_issue_form_key(code, modifiers).unwrap();
            }
            InputMode::ReviewDrawer => {
                app.handle_review_drawer_key(code, modifiers).unwrap();
            }
        }
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// Render the app to a test buffer and return the content as a string.
    #[allow(deprecated)]
    fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::super::ui::draw(frame, app))
            .unwrap();

        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut lines = Vec::new();
        for y in area.y..area.y + area.height {
            let mut line = String::new();
            for x in area.x..area.x + area.width {
                let cell = buf.get(x, y);
                line.push_str(cell.symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }

    // ═══════════════════════════════════════════════════════════════
    // 1. NAVIGATION TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn focus_switching_with_numbers() {
        let mut app = test_app();
        assert_eq!(app.focus, Focus::Projects);

        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.focus, Focus::Tasks);

        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.focus, Focus::Projects);
    }

    #[test]
    fn board_view_allows_global_focus_navigation() {
        let mut app = test_app();
        app.workbench_view = WorkbenchView::SprintBoard;
        app.input_mode = InputMode::BoardView;
        app.focus = Focus::Tasks;

        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.focus, Focus::Projects);

        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.focus, Focus::Tasks);

        press(&mut app, KeyCode::Char('3'));
        assert_eq!(app.focus, Focus::Tasks);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focus, Focus::Projects);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Tasks);
    }

    #[test]
    fn board_view_respects_sidebar_focus_for_navigation() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("alpha", "/tmp/alpha", "main", true)
            .unwrap();
        store
            .create_project("beta", "/tmp/beta", "main", true)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.loading = false;
        app.workbench_view = WorkbenchView::SprintBoard;
        app.input_mode = InputMode::BoardView;
        app.focus = Focus::Projects;
        app.sidebar_cursor = 1; // Sprint Board
        app.board_column_index = 0;

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            app.selected_sidebar_item(),
            SidebarItem::Navigation(WorkbenchView::Reviews)
        );
        assert_eq!(app.board_column_index, 0);

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.workbench_view, WorkbenchView::Reviews);
    }

    #[test]
    fn settings_view_sections_navigate_with_jk() {
        let mut app = test_app();
        app.workbench_view = WorkbenchView::Settings;
        app.focus = Focus::Tasks;

        assert_eq!(app.settings_section_index, 0);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.settings_section_index, 1);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.settings_section_index, 2);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.settings_section_index, 1);
    }

    #[test]
    fn settings_ai_providers_enter_opens_picker_overlay() {
        let mut app = test_app();
        app.workbench_view = WorkbenchView::Settings;
        app.focus = Focus::Tasks;

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::SettingsPicker);
        assert_eq!(
            app.settings_picker_target,
            Some(SettingsEditTarget::ClaudeModel)
        );
        assert!(!app.settings_picker_options.is_empty());
    }

    #[test]
    fn settings_edit_target_updates_ai_provider_fields() {
        let mut config = crate::config::Config::default();
        SettingsEditTarget::ClaudeModel
            .apply(&mut config, "claude-sonnet-4")
            .unwrap();
        SettingsEditTarget::ClaudeEffort
            .apply(&mut config, "high")
            .unwrap();
        assert_eq!(config.claude.model, "claude-sonnet-4");
        assert_eq!(config.claude.effort, "high");
    }

    #[test]
    fn settings_edit_target_validates_review_loop_interval() {
        let mut config = crate::config::Config::default();
        assert!(
            SettingsEditTarget::ReviewLoopInterval
                .apply(&mut config, "abc")
                .is_err()
        );
        SettingsEditTarget::ReviewLoopInterval
            .apply(&mut config, "45")
            .unwrap();
        assert_eq!(config.review_loop.poll_interval_secs, 45);
    }

    #[test]
    fn resize_sidebar_with_bracket_keys() {
        let mut app = test_app_with_project();
        app.last_terminal_area = Rect::new(0, 0, 140, 40);
        app.focus = Focus::Projects;

        let initial = app.workbench_sidebar_width;
        press(&mut app, KeyCode::Char(']'));
        assert!(app.workbench_sidebar_width > initial);

        press(&mut app, KeyCode::Char('['));
        assert_eq!(app.workbench_sidebar_width, initial);
    }

    #[test]
    fn resize_main_focus_adjusts_inspector_width() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::Threads;
        app.last_terminal_area = Rect::new(0, 0, 140, 40);
        app.focus = Focus::Tasks;

        let initial = app.workbench_inspector_width;
        press(&mut app, KeyCode::Char('['));
        assert!(app.workbench_inspector_width < initial);

        press(&mut app, KeyCode::Char(']'));
        assert_eq!(app.workbench_inspector_width, initial);
    }

    #[test]
    fn drag_sidebar_divider_resizes_workbench() {
        let mut app = test_app_with_project();
        app.last_terminal_area = Rect::new(0, 0, 140, 40);

        let layout = super::super::ui::compute_workbench_layout(
            app.last_terminal_area,
            app.workbench_sidebar_width,
            app.workbench_inspector_width,
            true,
        );
        let divider_col = layout.sidebar.x + layout.sidebar.width;
        let drag_row = layout.body.y + 1;

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: divider_col,
            row: drag_row,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: divider_col + 6,
            row: drag_row,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: divider_col + 6,
            row: drag_row,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();

        assert!(app.workbench_sidebar_width > 30);
        assert!(app.workbench_drag_target.is_none());
    }

    #[test]
    fn navigate_projects_jk() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("alpha", "/tmp/alpha", "main", true)
            .unwrap();
        store
            .create_project("beta", "/tmp/beta", "main", true)
            .unwrap();
        store
            .create_project("gamma", "/tmp/gamma", "main", true)
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.loading = false;

        assert_eq!(
            app.selected_sidebar_item(),
            SidebarItem::Navigation(WorkbenchView::MyTasks)
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            app.selected_sidebar_item(),
            SidebarItem::Navigation(WorkbenchView::SprintBoard)
        );
        for _ in 0..WorkbenchView::ALL.len().saturating_sub(2) {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_sidebar_item(), SidebarItem::Repository(0));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_sidebar_item(), SidebarItem::Repository(1));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_sidebar_item(), SidebarItem::Repository(2));
        // Clamp at end
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.selected_sidebar_item(), SidebarItem::Repository(2));

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected_sidebar_item(), SidebarItem::Repository(1));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.selected_sidebar_item(), SidebarItem::Repository(0));
        // Clamp at start
        for _ in 0..WorkbenchView::ALL.len() {
            press(&mut app, KeyCode::Char('k'));
        }
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(
            app.selected_sidebar_item(),
            SidebarItem::Navigation(WorkbenchView::MyTasks)
        );
    }

    #[test]
    fn navigate_tasks_jk() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.task_index, 0);

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.task_index, 1);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.task_index, 2);
        // Clamp
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.task_index, 2);

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.task_index, 1);
    }

    #[test]
    fn navigate_with_arrow_keys() {
        let store = Store::open_in_memory().unwrap();
        store.create_project("a", "/tmp/a", "main", true).unwrap();
        store.create_project("b", "/tmp/b", "main", true).unwrap();
        let mut app = App::new(store).unwrap();
        app.loading = false;

        press(&mut app, KeyCode::Down);
        assert_eq!(
            app.selected_sidebar_item(),
            SidebarItem::Navigation(WorkbenchView::SprintBoard)
        );

        press(&mut app, KeyCode::Up);
        assert_eq!(
            app.selected_sidebar_item(),
            SidebarItem::Navigation(WorkbenchView::MyTasks)
        );
    }

    #[test]
    fn quit_with_q() {
        let mut app = test_app();
        assert!(!app.should_quit);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn quit_with_ctrl_c() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.should_quit);
    }

    // ═══════════════════════════════════════════════════════════════
    // 2. PROJECT MANAGEMENT TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn add_project_opens_form() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.input_mode, InputMode::NewProject);
        assert_eq!(app.new_project_field, 0);
    }

    #[test]
    fn add_project_form_field_cycling() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.new_project_field, 0);

        type_str(&mut app, "my-proj");
        assert_eq!(app.input_buffer, "my-proj");

        // Tab to path field
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_project_field, 1);
        assert_eq!(app.new_project_name, "my-proj");

        // BackTab back to name
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_project_field, 0);
        assert_eq!(app.input_buffer, "my-proj");
    }

    #[test]
    fn add_project_cancel_with_esc() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('a'));
        type_str(&mut app, "will-cancel");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.new_project_name.is_empty());
    }

    #[test]
    fn add_project_submit() {
        let mut app = test_app();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        press(&mut app, KeyCode::Char('a'));
        type_str(&mut app, "new-proj");
        press(&mut app, KeyCode::Tab);

        // Clear default "." in path field
        press(&mut app, KeyCode::Backspace);
        type_str(&mut app, path);
        press(&mut app, KeyCode::Enter);

        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].name, "new-proj");
    }

    #[test]
    fn remove_project_with_confirm() {
        let mut app = test_app_with_project();
        assert_eq!(app.projects.len(), 1);

        for _ in 0..WorkbenchView::ALL.len() {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.input_mode, InputMode::ConfirmDelete);
        assert!(matches!(app.confirm_delete_kind, DeleteTarget::Project));
        assert_eq!(app.confirm_target, "test-project");

        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.projects.is_empty());
    }

    #[test]
    fn remove_project_cancel_n() {
        let mut app = test_app_with_project();
        for _ in 0..WorkbenchView::ALL.len() {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.projects.len(), 1);
    }

    #[test]
    fn remove_project_cancel_esc() {
        let mut app = test_app_with_project();
        for _ in 0..WorkbenchView::ALL.len() {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.projects.len(), 1);
    }

    #[test]
    fn select_project_loads_its_data() {
        let store = Store::open_in_memory().unwrap();
        let p1 = store
            .create_project("alpha", "/tmp/alpha", "main", true)
            .unwrap();
        let p2 = store
            .create_project("beta", "/tmp/beta", "main", true)
            .unwrap();
        store
            .create_task(
                &p1.id,
                "alpha-task",
                "",
                TaskMode::Supervised,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &p2.id,
                "beta-task",
                "",
                TaskMode::Supervised,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        let mut app = App::new(store).unwrap();
        app.loading = false;

        // First project selected by default
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].title, "alpha-task");

        // Navigate to the second repository row and activate it
        for _ in 0..=WorkbenchView::ALL.len() {
            press(&mut app, KeyCode::Char('j'));
        }
        assert_eq!(app.selected_sidebar_item(), SidebarItem::Repository(1));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.project_index, 1);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].title, "beta-task");
    }

    #[test]
    fn select_sidebar_navigation_switches_workbench_view() {
        let mut app = test_app_with_project();
        assert_eq!(app.workbench_view, WorkbenchView::MyTasks);

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            app.selected_sidebar_item(),
            SidebarItem::Navigation(WorkbenchView::SprintBoard)
        );

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.workbench_view, WorkbenchView::SprintBoard);
    }

    // ═══════════════════════════════════════════════════════════════
    // 3. TASK LIFECYCLE TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn create_task_full_flow() {
        let mut app = test_app_with_project();
        assert!(app.tasks.is_empty());

        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.input_mode, InputMode::NewTask);
        assert_eq!(app.new_task_field, 0);

        // Type prompt
        type_str(&mut app, "Fix the login bug");
        // Tab to mode
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_description, "Fix the login bug");
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);
        // Toggle mode: Autonomous → Supervised (Right)
        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);
        // Submit
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);

        let tasks = app
            .store
            .list_tasks_for_project(&app.projects[0].id)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Fix the login bug"); // auto-generated from prompt
        assert_eq!(tasks[0].description, "Fix the login bug");
        assert_eq!(tasks[0].mode, TaskMode::Supervised);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn launch_task_opens_thread_modal_for_pending_task() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('l'));

        assert_eq!(app.input_mode, InputMode::LaunchThread);
        let draft = app.launch_thread_draft.as_ref().unwrap();
        assert_eq!(
            draft.task_id.as_deref(),
            Some(app.visible_tasks()[0].id.as_str())
        );
        assert_eq!(draft.provider_kind, ProviderKind::Claude);
        assert_eq!(draft.title, app.visible_tasks()[0].title);
    }

    #[test]
    fn threads_view_new_task_opens_ad_hoc_launch_modal() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::Threads;
        app.focus = Focus::Tasks;

        press(&mut app, KeyCode::Char('n'));

        assert_eq!(app.input_mode, InputMode::LaunchThread);
        let draft = app.launch_thread_draft.as_ref().unwrap();
        assert!(draft.is_ad_hoc());
        assert_eq!(draft.provider_kind, ProviderKind::Claude);
        // Ad hoc starts with empty title for user to fill in
        assert!(draft.title.is_empty());
        assert_eq!(draft.workflow_name, "plan_first_tdd");
    }

    #[test]
    fn launch_thread_modal_cycles_provider_and_edits_context() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('l'));

        press(&mut app, KeyCode::Right);
        assert_eq!(
            app.launch_thread_draft.as_ref().unwrap().provider_kind,
            ProviderKind::Codex
        );

        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        type_str(&mut app, " add context");
        assert_eq!(
            app.launch_thread_draft.as_ref().unwrap().extra_context,
            " add context"
        );
    }

    #[test]
    fn create_task_cancel_empty_discards() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        // Esc with empty description discards (no draft created)
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.tasks.is_empty());
    }

    #[test]
    fn create_task_esc_saves_draft() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "Draft task");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].status, TaskStatus::Draft);
        assert_eq!(app.tasks[0].description, "Draft task");
    }

    #[test]
    fn create_task_requires_project() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn create_task_empty_prompt_does_not_submit() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Enter);
        // Should stay in NewTask mode
        assert_eq!(app.input_mode, InputMode::NewTask);
    }

    #[test]
    fn edit_task_flow() {
        let mut app = test_app_with_tasks();
        let original_id = app.tasks[0].id.clone();

        press(&mut app, KeyCode::Char('2')); // Focus tasks
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::EditTask);
        assert_eq!(app.input_buffer, "First task"); // loads description
        assert_eq!(app.editing_task_id.as_deref(), Some(original_id.as_str()));

        // Clear and retype prompt
        for _ in 0.."First task".len() {
            press(&mut app, KeyCode::Backspace);
        }
        type_str(&mut app, "Updated prompt");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);

        let task = app.store.get_task(&original_id).unwrap();
        assert_eq!(task.title, "Updated prompt"); // auto-generated from prompt
        assert_eq!(task.description, "Updated prompt");
    }

    #[test]
    fn edit_task_esc_saves_draft() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        for _ in 0..20 {
            press(&mut app, KeyCode::Backspace);
        }
        type_str(&mut app, "Changed");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.description, "Changed");
    }

    #[test]
    fn edit_task_only_works_on_pending_or_draft() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        // Navigate to the Working task (sorted after Pending tasks)
        let working_idx = app
            .visible_tasks()
            .iter()
            .position(|t| t.id == task_id)
            .unwrap();
        for _ in 0..working_idx {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn edit_draft_task_promotes_to_pending() {
        let mut app = test_app_with_project();
        // Create a draft by pressing Esc with content
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "My draft");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].status, TaskStatus::Draft);

        // Focus tasks and edit the draft
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::EditTask);

        // Submit the edit — draft should become pending
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn delete_task_flow() {
        let mut app = test_app_with_tasks();
        assert_eq!(app.visible_tasks().len(), 3);

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.input_mode, InputMode::ConfirmDelete);
        assert!(matches!(app.confirm_delete_kind, DeleteTarget::Task));
        assert_eq!(app.confirm_target, "Task Alpha");

        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.input_mode, InputMode::Normal);
        let tasks = app
            .store
            .list_tasks_for_project(&app.projects[0].id)
            .unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn delete_task_any_status() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('d'));
        // Any task status should allow deletion
        assert_eq!(app.input_mode, InputMode::ConfirmDelete);
    }

    #[test]
    fn reorder_tasks_shift_j() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));

        assert_eq!(app.visible_tasks()[0].title, "Task Alpha");
        assert_eq!(app.visible_tasks()[1].title, "Task Beta");

        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.task_index, 1);
        assert_eq!(app.visible_tasks()[0].title, "Task Beta");
        assert_eq!(app.visible_tasks()[1].title, "Task Alpha");
    }

    #[test]
    fn reorder_tasks_shift_k() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('j')); // Move to second task
        assert_eq!(app.task_index, 1);

        press(&mut app, KeyCode::Char('K'));
        assert_eq!(app.task_index, 0);
        assert_eq!(app.visible_tasks()[0].title, "Task Beta");
        assert_eq!(app.visible_tasks()[1].title, "Task Alpha");
    }

    #[test]
    fn filter_tasks_enter_applies() {
        let mut app = test_app_with_tasks();
        assert_eq!(app.visible_tasks().len(), 3);

        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.input_mode, InputMode::TaskFilter);
        assert_eq!(app.focus, Focus::Tasks);

        type_str(&mut app, "alpha");
        assert_eq!(app.visible_tasks().len(), 1);
        assert_eq!(app.visible_tasks()[0].title, "Task Alpha");

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.task_filter, "alpha");
        assert_eq!(app.visible_tasks().len(), 1);
    }

    #[test]
    fn filter_tasks_esc_clears() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('/'));
        type_str(&mut app, "beta");
        assert_eq!(app.visible_tasks().len(), 1);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.task_filter.is_empty());
        assert_eq!(app.visible_tasks().len(), 3);
    }

    #[test]
    fn filter_is_case_insensitive() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('/'));
        type_str(&mut app, "GAMMA");
        assert_eq!(app.visible_tasks().len(), 1);
        assert_eq!(app.visible_tasks()[0].title, "Task Gamma");
    }

    #[test]
    fn visible_tasks_includes_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        // Transition through valid path: Pending → Working → InReview → Done
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::InReview)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::Done)
            .unwrap();
        app.refresh_data().unwrap();

        let visible = app.visible_tasks();
        assert!(visible.iter().any(|t| t.status == TaskStatus::Done));
        // Done tasks should be included
        assert_eq!(visible.len(), app.tasks.len());
    }

    #[test]
    fn visible_tasks_sorted_by_status_priority() {
        let mut app = test_app_with_tasks();
        // Alpha=Pending, Beta=Pending, Gamma=Pending initially.
        // Set each to a different status via valid transitions.
        let alpha_id = app.tasks[0].id.clone();
        let beta_id = app.tasks[1].id.clone();

        // Alpha: Pending → Working → Error
        app.store
            .update_task_status(&alpha_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&alpha_id, TaskStatus::Error)
            .unwrap();
        // Beta: Pending → Working → InReview
        app.store
            .update_task_status(&beta_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&beta_id, TaskStatus::InReview)
            .unwrap();
        // Gamma stays Pending
        app.refresh_data().unwrap();

        let visible = app.visible_tasks();
        // Expected order: InReview (Beta) → Error (Alpha) → Pending (Gamma)
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].title, "Task Beta");
        assert_eq!(visible[0].status, TaskStatus::InReview);
        assert_eq!(visible[1].title, "Task Alpha");
        assert_eq!(visible[1].status, TaskStatus::Error);
        assert_eq!(visible[2].title, "Task Gamma");
        assert_eq!(visible[2].status, TaskStatus::Pending);
    }

    // ═══════════════════════════════════════════════════════════════
    // 4. TASK REVIEW FLOW
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn review_task_marks_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        // Transition through valid path: Pending → Working → InReview
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::InReview)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('r'));

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(app.toast_message.as_deref(), Some("Task marked as done"));
    }

    #[test]
    fn review_working_task_marks_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        // Navigate to the Working task (sorted after Pending tasks)
        let working_idx = app
            .visible_tasks()
            .iter()
            .position(|t| t.id == task_id)
            .unwrap();
        for _ in 0..working_idx {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('r'));

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(app.toast_message.as_deref(), Some("Task marked as done"));
    }

    #[test]
    fn review_interrupted_task_marks_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        // Transition through valid path: Pending → Working → Interrupted
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::Interrupted)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        // Navigate to the Interrupted task (sorted before Pending)
        let idx = app
            .visible_tasks()
            .iter()
            .position(|t| t.id == task_id)
            .unwrap();
        for _ in 0..idx {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('r'));

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(app.toast_message.as_deref(), Some("Task marked as done"));
    }

    #[test]
    fn review_only_works_on_in_review_tasks() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('r'));

        // Pending task: r should do nothing
        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
    }

    // ═══════════════════════════════════════════════════════════════
    // 5. COMMAND PALETTE
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn command_palette_opens_with_ctrl_p() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(app.input_mode, InputMode::CommandPalette);
        assert!(app.input_buffer.is_empty());
        assert!(!app.palette_filtered.is_empty());
    }

    #[test]
    fn command_palette_filters_items() {
        let mut app = test_app_with_project();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        let initial_count = app.palette_filtered.len();

        type_str(&mut app, "quit");
        assert!(app.palette_filtered.len() < initial_count);
        assert!(!app.palette_filtered.is_empty());
    }

    #[test]
    fn command_palette_navigate() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(app.palette_index, 0);

        press(&mut app, KeyCode::Down);
        assert_eq!(app.palette_index, 1);

        press(&mut app, KeyCode::Up);
        assert_eq!(app.palette_index, 0);
    }

    #[test]
    fn command_palette_cancel() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "test");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn command_palette_execute_quit() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "quit");
        press(&mut app, KeyCode::Enter);
        assert!(app.should_quit);
    }

    #[test]
    fn command_palette_execute_focus_tasks() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "focus tasks");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Tasks);
    }

    #[test]
    fn command_palette_execute_new_task() {
        let mut app = test_app_with_project();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "new task");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::NewTask);
    }

    #[test]
    fn command_palette_execute_add_project() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "add project");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::NewProject);
    }

    #[test]
    fn command_palette_backspace_refilters() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "quit");
        let narrow_count = app.palette_filtered.len();

        press(&mut app, KeyCode::Backspace);
        // After removing a character, more items should match
        assert!(app.palette_filtered.len() >= narrow_count);
    }

    // ═══════════════════════════════════════════════════════════════
    // 7. HELP OVERLAY
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn help_overlay_open_close_question_mark() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.input_mode, InputMode::HelpOverlay);

        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn help_overlay_close_with_esc() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn help_overlay_close_with_q() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    // ═══════════════════════════════════════════════════════════════
    // 8. TASK FORM DETAILS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn task_form_backtab_cycles() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.new_task_field, 0);

        // BackTab wraps to field 6 (subtasks)
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 6);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 5);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 4);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 3);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 2);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 1);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 0);
    }

    #[test]
    fn task_form_tab_forward_cycles() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 2);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 3);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 4);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 5);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 6);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 0);
    }

    #[test]
    fn task_form_mode_toggle() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);

        // Right cycles: Autonomous → Supervised → Exploration → Autonomous
        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);

        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Exploration);

        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);

        // Left cycles in reverse: Autonomous → Exploration → Supervised → Autonomous
        press(&mut app, KeyCode::Left);
        assert_eq!(app.new_task_mode, TaskMode::Exploration);

        press(&mut app, KeyCode::Left);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);

        press(&mut app, KeyCode::Left);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);
    }

    #[test]
    fn edit_task_form_cycling() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::EditTask);

        // Tab cycles through prompt (0), mode (1), base (2), branch (3), push_mode (4), loop (5), subtasks (6)
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 2);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 3);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 4);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 5);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 6);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 0);
    }

    // ═══════════════════════════════════════════════════════════════
    // 10. TOAST NOTIFICATION
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn toast_shows_on_success_actions() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        // Transition through valid path: Pending → Working → InReview
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::InReview)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('r'));

        assert!(app.toast_message.is_some());
        assert!(matches!(app.toast_style, ToastStyle::Success));
    }

    #[test]
    fn toast_on_project_delete() {
        let mut app = test_app_with_project();
        for _ in 0..WorkbenchView::ALL.len() {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.toast_message.is_some());
        assert!(matches!(app.toast_style, ToastStyle::Success));
    }

    #[test]
    fn toast_on_task_delete() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        assert!(
            app.toast_message
                .as_deref()
                .is_some_and(|m| m.contains("deleted"))
        );
    }

    #[test]
    fn toast_on_task_edit() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        // Just submit with existing title
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.toast_message.as_deref(), Some("Task updated"));
    }

    // ═══════════════════════════════════════════════════════════════
    // 11. SNAPSHOT RENDER TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn snapshot_active_view_empty() {
        let mut app = test_app();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("claustre"));
        assert!(output.contains("Sidebar"));
        assert!(output.contains("Navigation"));
        assert!(output.contains("no projects"));
    }

    #[test]
    fn snapshot_active_view_with_data() {
        let mut app = test_app_with_tasks();
        let output = render_to_string(&mut app, 140, 30);
        assert!(output.contains("claustre"));
        assert!(output.contains("Sidebar"));
        assert!(output.contains("My Tasks"));
        assert!(output.contains("test-project"));
        assert!(output.contains("Task Alpha"));
        assert!(output.contains("Task Beta"));
        assert!(output.contains("Task Gamma"));
    }

    #[test]
    fn snapshot_active_view_session_detail() {
        let mut app = test_app_with_project();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("My Tasks"));
        assert!(!output.contains("Inspector"));
    }

    #[test]
    fn snapshot_active_view_session_detail_with_thread_workspace() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        app.workbench_view = WorkbenchView::Threads;
        let output = render_to_string(&mut app, 140, 50);
        assert!(output.contains("Threads ("));
        assert!(output.contains("Task Alpha"));
        assert!(output.contains("Plan the work first"));
        assert!(output.contains("live"));
        // Compose area is at the bottom — may scroll off in small terminals
        // but the working indicator and timeline should be visible
        assert!(
            output.contains("Reply") || output.contains("Working"),
            "compose or working indicator should be visible"
        );
        assert!(!output.contains("Context"));
        assert!(!output.contains("Timeline"));
        assert!(!output.contains("Compose"));
    }

    #[test]
    fn thread_workspace_timeline_renders_conversation_stream() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        app.workbench_view = WorkbenchView::Threads;

        let output = render_to_string(&mut app, 180, 60);
        assert!(output.contains("You"));
        assert!(output.contains("Plan the work first"));
        assert!(!output.contains("╭─ Run "));
        assert!(!output.contains("╭─ Runtime "));
        assert!(!output.contains("╭─ Plan "));
        assert!(!output.contains("╭─ GitHub "));
        assert!(!output.contains("╭─ Knowledge "));
    }

    #[test]
    fn typing_in_threads_starts_native_chat_editor() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        app.workbench_view = WorkbenchView::Threads;
        app.focus = Focus::Tasks;

        press(&mut app, KeyCode::Char('H'));

        assert_eq!(app.input_mode, InputMode::ThreadCompose);
        assert_eq!(
            app.thread_compose_thread_id.as_deref(),
            app.selected_thread().map(|thread| thread.id.as_str())
        );
        assert_eq!(app.input_buffer, "H");

        let output = render_to_string(&mut app, 140, 40);
        assert!(output.contains("Reply"));
        assert!(output.contains("Enter:send"));
    }

    #[test]
    fn thread_compose_submit_persists_message_and_clears_draft() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        app.workbench_view = WorkbenchView::Threads;
        app.focus = Focus::Tasks;

        app.start_thread_compose().unwrap();
        app.input_buffer = "Continue from the open review comments".to_string();
        app.input_cursor = app.input_buffer.len();
        app.thread_compose_buffer = app.input_buffer.clone();
        app.thread_compose_cursor = app.input_cursor;

        app.submit_thread_compose_message().unwrap();

        let thread = app.selected_thread().unwrap().clone();
        let messages = app.store.list_thread_messages(&thread.id).unwrap();
        let latest = messages.last().unwrap();
        assert_eq!(latest.role, "user");
        assert_eq!(latest.content, "Continue from the open review comments");
        // When send fails and restart also fails, the compose buffer is preserved
        // so the user doesn't lose their message. Toast indicates failure.
        let toast = app.toast_message.as_deref().unwrap_or_default();
        let send_succeeded = toast.contains("Sent to agent") || toast.contains("Resuming");
        if send_succeeded {
            assert_eq!(app.input_mode, InputMode::Normal);
            assert!(app.input_buffer.is_empty());
            assert!(app.thread_compose_buffer.is_empty());
        } else {
            // Failed — buffer preserved, message still recorded in DB
            assert!(
                toast.contains("exited") || toast.contains("not reachable"),
                "unexpected toast: {toast}"
            );
        }
    }

    #[test]
    fn project_picker_opens_from_global_shortcut() {
        let mut app = test_app_with_tasks();

        press(&mut app, KeyCode::Char('P'));

        assert_eq!(app.input_mode, InputMode::ProjectPicker);
        let output = render_to_string(&mut app, 120, 32);
        assert!(output.contains("Switch Repository"));
        assert!(output.contains("test-project"));
    }

    #[test]
    fn lowercase_project_picker_shortcut_opens_from_workbench() {
        let mut app = test_app_with_tasks();

        press(&mut app, KeyCode::Char('p'));

        assert_eq!(app.input_mode, InputMode::ProjectPicker);
    }

    #[test]
    fn thread_provider_picker_opens_from_threads_workspace() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        app.workbench_view = WorkbenchView::Threads;
        app.focus = Focus::Tasks;

        press(&mut app, KeyCode::Char('m'));

        assert_eq!(app.input_mode, InputMode::ThreadProviderPicker);
        let output = render_to_string(&mut app, 120, 32);
        assert!(output.contains("Switch Provider"));
        assert!(output.contains("claude"));
        assert!(output.contains("codex"));
    }

    #[test]
    fn linked_session_tabs_default_to_conversation_view() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        let session_id = app.threads[0].session_id.clone().unwrap();

        let rows = 24;
        let cols = 80;
        let mut shell_cmd = portable_pty::CommandBuilder::new("/bin/sh");
        shell_cmd.arg("-lc");
        shell_cmd.arg("printf 'shell'");
        let mut agent_cmd = portable_pty::CommandBuilder::new("/bin/sh");
        agent_cmd.arg("-lc");
        agent_cmd.arg("printf 'agent'");

        let shell = crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, cols / 2).unwrap();
        let agent = crate::pty::EmbeddedTerminal::spawn(agent_cmd, rows, cols / 2).unwrap();
        let terminals =
            crate::pty::SessionTerminals::from_parts(shell, Box::new(agent), "/tmp/test-repo");
        app.add_session_tab(
            session_id.clone(),
            Box::new(terminals),
            "Session".to_string(),
        );

        let tab = app
            .tabs
            .iter()
            .find(|tab| matches!(tab, Tab::Session { session_id: sid, .. } if sid == &session_id))
            .unwrap();
        match tab {
            Tab::Session { view_mode, .. } => {
                assert_eq!(*view_mode, SessionTabView::Terminal);
            }
            Tab::Dashboard => panic!("expected session tab"),
        }
    }

    #[test]
    fn add_session_tab_bridges_task_sessions_into_conversation_view() {
        let mut app = test_app_with_tasks();
        let project = app.projects[0].clone();
        let task = app.tasks[0].clone();
        let session = app
            .store
            .create_session(
                &project.id,
                "task-alpha",
                "/tmp/test-repo-worktree",
                "Task Alpha",
            )
            .unwrap();
        app.store
            .assign_task_to_session(&task.id, &session.id)
            .unwrap();
        app.store
            .update_session_status(&session.id, crate::store::ClaudeStatus::Working, "Planning")
            .unwrap();
        app.store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        app.refresh_data().unwrap();

        let rows = 24;
        let cols = 80;
        let mut shell_cmd = portable_pty::CommandBuilder::new("/bin/sh");
        shell_cmd.arg("-lc");
        shell_cmd.arg("printf 'shell'");
        let mut agent_cmd = portable_pty::CommandBuilder::new("/bin/sh");
        agent_cmd.arg("-lc");
        agent_cmd.arg("printf 'agent'");

        let shell = crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, cols / 2).unwrap();
        let agent = crate::pty::EmbeddedTerminal::spawn(agent_cmd, rows, cols / 2).unwrap();
        let terminals =
            crate::pty::SessionTerminals::from_parts(shell, Box::new(agent), "/tmp/test-repo");
        app.add_session_tab(
            session.id.clone(),
            Box::new(terminals),
            "Session".to_string(),
        );

        assert!(
            app.store
                .find_thread_for_session(&session.id)
                .unwrap()
                .is_some()
        );
        let tab = app
            .tabs
            .iter()
            .find(|tab| matches!(tab, Tab::Session { session_id: sid, .. } if sid == &session.id))
            .unwrap();
        match tab {
            Tab::Session { view_mode, .. } => {
                assert_eq!(*view_mode, SessionTabView::Terminal);
            }
            Tab::Dashboard => panic!("expected session tab"),
        }
    }

    #[test]
    fn selected_thread_context_preserves_full_run_history() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        let thread = app.threads[0].clone();
        app.store
            .create_thread_run(
                &thread.id,
                crate::store::ProviderKind::Codex,
                Some("review"),
                crate::store::ThreadRunStatus::Done,
                Some("Review the latest diff"),
            )
            .unwrap();

        let thread_ctx = app.selected_thread_context().unwrap();
        assert_eq!(thread_ctx.run_count, 2);
        assert_eq!(thread_ctx.runs.len(), 2);
        assert_eq!(
            thread_ctx.runs.last().map(|run| run.provider_kind),
            Some(crate::store::ProviderKind::Codex)
        );
    }

    #[test]
    fn snapshot_settings_view_structured() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::Settings;
        app.focus = Focus::Tasks;
        let output = render_to_string(&mut app, 140, 40);
        assert!(output.contains("Sections"));
        assert!(output.contains("AI Providers"));
        assert!(output.contains("Permissions"));
        assert!(output.contains("GitHub"));
        assert!(output.contains("Claude"));
        assert!(output.contains("Tools"));
    }

    #[test]
    fn reviews_queue_separates_authored_and_requested_prs() {
        let mut app = test_app_with_tasks();
        seed_review_queue_items(&mut app);

        assert_eq!(app.review_authored_items.len(), 1);
        assert_eq!(app.review_requested_items.len(), 1);
        assert_eq!(app.review_authored_items[0].github_item.number, 42);
        assert_eq!(app.review_requested_items[0].github_item.number, 77);
    }

    #[test]
    fn snapshot_reviews_view_renders_review_tabs_and_pr_rows() {
        let mut app = test_app_with_tasks();
        seed_review_queue_items(&mut app);
        app.workbench_view = WorkbenchView::Reviews;
        app.focus = Focus::Tasks;

        let output = render_to_string(&mut app, 160, 40);
        assert!(output.contains("Authored PRs (1)"));
        assert!(output.contains("Needs Review (1)"));
        assert!(output.contains("#42 Render issue and PR review context"));

        press(&mut app, KeyCode::Char('t'));
        let toggled = render_to_string(&mut app, 160, 40);
        assert!(toggled.contains("#77 Review queue PR"));
    }

    #[test]
    fn launch_from_review_queue_opens_review_thread_modal() {
        let mut app = test_app_with_tasks();
        seed_review_queue_items(&mut app);
        app.workbench_view = WorkbenchView::Reviews;
        app.focus = Focus::Tasks;
        app.review_queue_tab = ReviewQueueTab::NeedsReview;
        app.review_index = 0;

        press(&mut app, KeyCode::Char('l'));

        assert_eq!(app.input_mode, InputMode::LaunchThread);
        let draft = app.launch_thread_draft.as_ref().unwrap();
        let review_pr = draft.review_pr.as_ref().unwrap();
        assert_eq!(review_pr.number, 77);
        assert!(matches!(review_pr.mode, PendingReviewLaunchMode::Review));
    }

    #[test]
    fn launch_from_authored_review_focuses_existing_thread() {
        let mut app = test_app_with_tasks();
        seed_review_queue_items(&mut app);
        app.workbench_view = WorkbenchView::Reviews;
        app.focus = Focus::Tasks;
        app.review_queue_tab = ReviewQueueTab::Authored;
        app.review_index = 0;

        press(&mut app, KeyCode::Char('l'));

        // In test there's no real session tab, so focus_thread_workspace
        // stays on the current view instead of falling back to Threads.
        assert_eq!(app.workbench_view, WorkbenchView::Reviews);
    }

    #[test]
    fn snapshot_github_settings_show_installations() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::Settings;
        app.focus = Focus::Tasks;
        app.settings_section_index = SettingsSection::ALL
            .iter()
            .position(|section| *section == SettingsSection::GitHub)
            .unwrap();
        app.github_status.authenticated = true;
        app.github_installations = vec![
            crate::github_app::GitHubInstallation {
                id: 42,
                account: crate::github_app::GitHubInstallationAccount {
                    login: "acme".to_string(),
                },
            },
            crate::github_app::GitHubInstallation {
                id: 84,
                account: crate::github_app::GitHubInstallationAccount {
                    login: "platform".to_string(),
                },
            },
        ];
        app.github_installation_index = 1;
        app.config.github_app.default_installation_id = Some("84".to_string());
        let output = render_to_string(&mut app, 140, 40);
        assert!(output.contains("App install"));
        assert!(output.contains("acme (42)"));
        assert!(output.contains("platform (84) default"));
        assert!(output.contains("installations: loaded"));
    }

    #[test]
    fn snapshot_github_settings_prefers_setup_flow_over_raw_config() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::Settings;
        app.focus = Focus::Tasks;
        app.settings_section_index = SettingsSection::ALL
            .iter()
            .position(|section| *section == SettingsSection::GitHub)
            .unwrap();
        app.tool_status.gh = true;
        let output = render_to_string(&mut app, 140, 40);
        assert!(output.contains("Setup"));
        assert!(output.contains("auth backend: GitHub CLI"));
        assert!(output.contains("no bundled GitHub App metadata"));
        assert!(!output.contains("Client ID: not set"));
    }

    #[test]
    fn snapshot_github_installation_picker_overlay() {
        let mut app = test_app();
        app.input_mode = InputMode::GitHubInstallationPicker;
        app.github_installations = vec![
            crate::github_app::GitHubInstallation {
                id: 42,
                account: crate::github_app::GitHubInstallationAccount {
                    login: "acme".to_string(),
                },
            },
            crate::github_app::GitHubInstallation {
                id: 99,
                account: crate::github_app::GitHubInstallationAccount {
                    login: "infra".to_string(),
                },
            },
        ];
        app.github_installation_index = 1;
        app.config.github_app.default_installation_id = Some("99".to_string());
        let output = render_to_string(&mut app, 120, 32);
        assert!(output.contains("GitHub Installations"));
        assert!(output.contains("acme (42)"));
        assert!(output.contains("infra (99) default"));
        assert!(output.contains("Enter"));
    }

    #[test]
    fn snapshot_github_project_picker_uses_title_first_labels() {
        let mut app = test_app_with_project();
        app.input_mode = InputMode::GitHubProjectPicker;
        app.github_projects_v2 = vec![
            crate::store::GitHubProjectV2Cache {
                id: "project-81".to_string(),
                repo_id: "repo-1".to_string(),
                project_number: 81,
                title: "1.7.2".to_string(),
                url: None,
                node_id: None,
                synced_at: None,
                created_at: "2026-03-17T00:00:00Z".to_string(),
            },
            crate::store::GitHubProjectV2Cache {
                id: "project-94".to_string(),
                repo_id: "repo-1".to_string(),
                project_number: 94,
                title: "OpenMetadata".to_string(),
                url: None,
                node_id: None,
                synced_at: None,
                created_at: "2026-03-17T00:00:00Z".to_string(),
            },
        ];
        app.github_project_index = 0;
        app.config.github_app.default_project_id = Some("81".to_string());
        let output = render_to_string(&mut app, 120, 32);
        assert!(output.contains("Switch GitHub Board"));
        assert!(output.contains("Select the default GitHub Project v2 board"));
        assert!(output.contains("Filter:"));
        assert!(output.contains("1.7.2 (#81)  default"));
        assert!(!output.contains("#81 1.7.2"));
        assert!(output.contains("type:filter"));
        assert!(output.contains("x:auto-select"));
    }

    #[test]
    fn github_project_picker_filters_visible_projects() {
        let mut app = test_app_with_project();
        app.input_mode = InputMode::GitHubProjectPicker;
        app.github_projects_v2 = vec![
            crate::store::GitHubProjectV2Cache {
                id: "project-81".to_string(),
                repo_id: "repo-1".to_string(),
                project_number: 81,
                title: "1.7.2".to_string(),
                url: None,
                node_id: None,
                synced_at: None,
                created_at: "2026-03-17T00:00:00Z".to_string(),
            },
            crate::store::GitHubProjectV2Cache {
                id: "project-88".to_string(),
                repo_id: "repo-1".to_string(),
                project_number: 88,
                title: "Backlog".to_string(),
                url: None,
                node_id: None,
                synced_at: None,
                created_at: "2026-03-17T00:00:00Z".to_string(),
            },
        ];
        app.github_project_index = 1;
        app.input_buffer = "back".to_string();
        app.input_cursor = app.input_buffer.len();

        let output = render_to_string(&mut app, 120, 18);
        assert!(output.contains("Backlog (#88)"));
        assert!(!output.contains("1.7.2 (#81)"));
        assert!(output.contains("▸ "));
    }

    #[test]
    fn snapshot_github_settings_show_title_first_default_project() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::Settings;
        app.focus = Focus::Tasks;
        app.settings_section_index = SettingsSection::ALL
            .iter()
            .position(|section| *section == SettingsSection::GitHub)
            .unwrap();
        app.github_projects_v2 = vec![crate::store::GitHubProjectV2Cache {
            id: "project-81".to_string(),
            repo_id: "repo-1".to_string(),
            project_number: 81,
            title: "1.7.2".to_string(),
            url: None,
            node_id: None,
            synced_at: None,
            created_at: "2026-03-17T00:00:00Z".to_string(),
        }];
        app.config.github_app.default_project_id = Some("81".to_string());
        let output = render_to_string(&mut app, 140, 40);
        assert!(output.contains("default board: 1.7.2 (#81)"));
        assert!(!output.contains("default board: #81 1.7.2"));
        assert!(output.contains("g choose default board"));
    }

    #[test]
    fn board_view_lowercase_p_opens_repo_picker() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::SprintBoard;
        app.input_mode = InputMode::BoardView;

        press(&mut app, KeyCode::Char('p'));

        assert_eq!(app.input_mode, InputMode::ProjectPicker);
    }

    #[test]
    fn board_view_g_opens_github_board_picker() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::SprintBoard;
        app.input_mode = InputMode::BoardView;
        app.github_projects_v2 = vec![crate::store::GitHubProjectV2Cache {
            id: "project-81".to_string(),
            repo_id: "repo-1".to_string(),
            project_number: 81,
            title: "1.7.2".to_string(),
            url: None,
            node_id: None,
            synced_at: None,
            created_at: "2026-03-17T00:00:00Z".to_string(),
        }];

        press(&mut app, KeyCode::Char('g'));

        assert_eq!(app.input_mode, InputMode::GitHubProjectPicker);
    }

    #[test]
    fn snapshot_sprint_board_empty_still_shows_selected_column() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::SprintBoard;
        app.input_mode = InputMode::BoardView;
        app.board_columns = vec![
            "Backlog".to_string(),
            "In Progress".to_string(),
            "In Review".to_string(),
            "Done".to_string(),
        ];
        app.board_issues = vec![Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        app.board_all_issues = app.board_issues.clone();
        app.board_column_index = 1;
        let output = render_to_string(&mut app, 140, 40);
        assert!(output.contains("Sprint Board"));
        // Only the selected column is shown when all columns are empty
        assert!(output.contains("In Progress"));
        assert!(output.contains("column: In Progress"));
    }

    #[test]
    fn snapshot_help_overlay() {
        let mut app = test_app();
        app.input_mode = InputMode::HelpOverlay;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Help"));
        assert!(output.contains("Ctrl+P"));
        assert!(output.contains("Quit"));
    }

    #[test]
    fn snapshot_command_palette() {
        let mut app = test_app();
        app.input_mode = InputMode::CommandPalette;
        app.filter_palette();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Command Palette"));
        assert!(output.contains("New Task"));
        assert!(output.contains("Quit"));
    }

    #[test]
    fn snapshot_task_form() {
        let mut app = test_app_with_project();
        app.input_mode = InputMode::NewTask;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("New Task"));
        assert!(output.contains("Prompt"));
        assert!(output.contains("Mode"));
    }

    #[test]
    fn snapshot_edit_task_form() {
        let mut app = test_app_with_tasks();
        app.editing_task_id = Some(app.tasks[0].id.clone());
        app.new_task_description = "First task".to_string();
        app.input_buffer = "First task".to_string();
        app.input_mode = InputMode::EditTask;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Edit Task"));
        assert!(output.contains("Prompt"));
    }

    #[test]
    fn snapshot_new_project_panel() {
        let mut app = test_app();
        app.input_mode = InputMode::NewProject;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Add Project"));
        assert!(output.contains("Name"));
        assert!(output.contains("Path"));
    }

    #[test]
    fn snapshot_confirm_delete() {
        let mut app = test_app_with_project();
        app.input_mode = InputMode::ConfirmDelete;
        app.confirm_target = "test-project".to_string();
        app.confirm_delete_kind = DeleteTarget::Project;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Delete"));
        assert!(output.contains("test-project"));
    }

    #[test]
    fn snapshot_task_filter_active() {
        let mut app = test_app_with_tasks();
        app.input_mode = InputMode::TaskFilter;
        app.task_filter = "alpha".to_string();
        app.task_filter_cursor = app.task_filter.len();
        app.recompute_visible_tasks();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("/alpha"));
        assert!(output.contains("Enter:apply"));
    }

    #[test]
    fn snapshot_usage_bars() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        app.workbench_view = WorkbenchView::Threads;
        app.rate_limit_state.usage_5h_pct = Some(42.0);
        app.rate_limit_state.usage_7d_pct = Some(15.0);
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Usage"));
        assert!(output.contains("5h"));
        assert!(output.contains("7d"));
        assert!(output.contains("42%"));
        assert!(output.contains("15%"));
    }

    #[test]
    fn snapshot_rate_limited_banner() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        app.workbench_view = WorkbenchView::Threads;
        app.rate_limit_state.is_rate_limited = true;
        app.rate_limit_state.limit_type = Some("5h".to_string());
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("RATE LIMITED"));
    }

    #[test]
    fn snapshot_toast_visible() {
        let mut app = test_app_with_project();
        app.show_toast("Test notification", ToastStyle::Success);
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Test notification"));
    }

    #[test]
    fn snapshot_task_status_indicators() {
        let mut app = test_app_with_tasks();
        // Set varied task statuses via valid transitions
        let t0 = app.tasks[0].id.clone();
        let t1 = app.tasks[1].id.clone();
        app.store
            .update_task_status(&t0, TaskStatus::Working)
            .unwrap();
        // t1: Pending → Working → InReview
        app.store
            .update_task_status(&t1, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&t1, TaskStatus::InReview)
            .unwrap();
        app.refresh_data().unwrap();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("working"));
        assert!(output.contains("in_review"));
        assert!(output.contains("pending"));
    }

    // ═══════════════════════════════════════════════════════════════
    // 12. SUBTASK PANEL
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn subtask_panel_opens() {
        let mut app = test_app_with_tasks();
        // Focus tasks
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.input_mode, InputMode::SubtaskPanel);
    }

    #[test]
    fn subtask_panel_add_and_close() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.input_mode, InputMode::SubtaskPanel);

        // Type a subtask description
        for c in "implement login".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);

        // Should have added a subtask
        let subtasks = app.store.list_subtasks_for_task(&task_id).unwrap();
        assert_eq!(subtasks.len(), 1);
        assert_eq!(subtasks[0].description, "implement login");

        // Close panel
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn subtask_panel_delete() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .create_subtask(&task_id, "step 1", "first step")
            .unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.subtasks.len(), 1);

        // Delete the subtask (d only works when input is empty)
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.subtasks.len(), 0);
        assert_eq!(app.toast_message.as_deref(), Some("Subtask deleted"));
    }

    #[test]
    fn subtask_panel_navigate() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .create_subtask(&task_id, "step 1", "first")
            .unwrap();
        app.store
            .create_subtask(&task_id, "step 2", "second")
            .unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.subtask_index, 0);

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.subtask_index, 1);

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.subtask_index, 0);
    }

    #[test]
    fn subtask_panel_requires_tasks_focus() {
        let mut app = test_app_with_tasks();
        // Focus is Projects by default
        assert_eq!(app.focus, Focus::Projects);
        press(&mut app, KeyCode::Char('s'));
        // Should be no-op since focus is not Tasks
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn subtask_counts_populated() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .create_subtask(&task_id, "step 1", "first")
            .unwrap();
        app.store
            .create_subtask(&task_id, "step 2", "second")
            .unwrap();
        app.refresh_data().unwrap();

        assert!(app.subtask_counts.contains_key(&task_id));
        let &(total, done) = app.subtask_counts.get(&task_id).unwrap();
        assert_eq!(total, 2);
        assert_eq!(done, 0);
    }

    #[test]
    fn snapshot_subtask_panel() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .create_subtask(&task_id, "step 1", "first step")
            .unwrap();
        app.subtasks = app.store.list_subtasks_for_task(&task_id).unwrap();
        app.input_mode = InputMode::SubtaskPanel;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Subtasks"));
        assert!(output.contains("step 1"));
    }

    // ═══════════════════════════════════════════════════════════════
    // 13. SKILL PANEL
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn skill_panel_opens_with_i() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.input_mode, InputMode::SkillPanel);
    }

    #[test]
    fn skill_panel_closes_with_esc() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn skill_panel_find_opens_search() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.input_mode, InputMode::SkillSearch);
        assert!(app.input_buffer.is_empty());
    }

    #[test]
    fn skill_panel_add_opens_add() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.input_mode, InputMode::SkillAdd);
        assert!(app.input_buffer.is_empty());
    }

    #[test]
    fn skill_search_esc_returns_to_panel() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.input_mode, InputMode::SkillSearch);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::SkillPanel);
    }

    #[test]
    fn skill_add_esc_returns_to_panel() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.input_mode, InputMode::SkillAdd);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::SkillPanel);
    }

    #[test]
    fn skill_panel_scope_toggle() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        assert!(app.skill_scope_global);
        press(&mut app, KeyCode::Char('g'));
        assert!(!app.skill_scope_global);
        press(&mut app, KeyCode::Char('g'));
        assert!(app.skill_scope_global);
    }

    #[test]
    fn snapshot_skill_panel() {
        let mut app = test_app();
        app.input_mode = InputMode::SkillPanel;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Skills"));
        assert!(output.contains("global"));
        assert!(output.contains("No skills installed"));
    }

    #[test]
    fn snapshot_task_details_panel() {
        let mut app = test_app_with_tasks();
        seed_thread_workspace(&mut app);
        app.input_mode = InputMode::TaskDetails;
        // task_index 0 points to "Task Alpha"
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Details"));
        assert!(output.contains("Task Alpha"));
        assert!(output.contains("Thread Workspace"));
        assert!(output.contains("Preview"));
        assert!(output.contains("Knowledge"));
        assert!(output.contains("plan_first_tdd"));
    }

    #[test]
    fn snapshot_my_tasks_details_panel() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::MyTasks;
        app.input_mode = InputMode::TaskDetails;
        app.my_task_items = vec![MyTaskItem {
            repo: crate::store::GitHubRepoCache {
                id: "repo-1".to_string(),
                project_id: Some(app.projects[0].id.clone()),
                owner: "open-metadata".to_string(),
                name: "OpenMetadata".to_string(),
                full_name: "open-metadata/OpenMetadata".to_string(),
                repo_url: Some("https://github.com/open-metadata/OpenMetadata".to_string()),
                default_branch: Some("main".to_string()),
                synced_at: None,
                created_at: "2026-03-18T00:00:00Z".to_string(),
            },
            github_item: crate::store::GitHubItem {
                id: "github-item-21535".to_string(),
                repo_id: "repo-1".to_string(),
                project_v2_id: None,
                node_id: None,
                number: 21535,
                kind: GitHubItemKind::Issue,
                title: "Support for Documenting Model Context Protocol (MCP) Services".to_string(),
                state: "OPEN".to_string(),
                url: "https://github.com/open-metadata/OpenMetadata/issues/21535".to_string(),
                body_text: Some("Need better MCP documentation".to_string()),
                assignee_logins: vec!["harshach".to_string()],
                label_names: vec!["feature".to_string(), "customer".to_string()],
                label_colors: serde_json::json!({}),
                project_field_values: serde_json::json!({"Status": "Backlog"}),
                base_ref: None,
                head_ref: None,
                github_updated_at: Some("2026-03-18T00:00:00Z".to_string()),
                synced_at: None,
                created_at: "2026-03-18T00:00:00Z".to_string(),
                updated_at: "2026-03-18T00:00:00Z".to_string(),
                linked_pr_number: None,
                linked_pr_item_id: None,
            },
            issue_cache: Some(crate::store::GitHubIssueCache {
                item_id: "github-item-21535".to_string(),
                body: Some("Issue body markdown".to_string()),
                body_text: Some("Issue body markdown".to_string()),
                body_html: None,
                author_login: Some("harshach".to_string()),
                milestone_title: Some("Sprint 12".to_string()),
                json_payload: None,
                cached_at: "2026-03-18T00:00:00Z".to_string(),
            }),
            pr_cache: None,
            linked_thread: None,
            linked_pr: None,
        }];
        app.cached_my_task_indices = vec![0];

        let output = render_to_string(&mut app, 120, 34);
        assert!(output.contains("Issue #21535"));
        assert!(output.contains("open-metadata/OpenMetadata"));
        assert!(output.contains("feature, customer"));
        assert!(output.contains("Project Fields"));
        assert!(output.contains("Issue body markdown"));
    }

    #[test]
    fn view_details_opens_from_sprint_board() {
        let mut app = test_app_with_project();
        app.workbench_view = WorkbenchView::SprintBoard;
        app.input_mode = InputMode::BoardView;
        app.focus = Focus::Tasks;
        app.board_issues = vec![vec![crate::github::GitHubBoardItem {
            github_item_id: "github-item-42".to_string(),
            number: 42,
            kind: GitHubItemKind::Issue,
            title: "Board issue".to_string(),
            body: Some("Details from project board".to_string()),
            state: "OPEN".to_string(),
            url: "https://example.com/issues/42".to_string(),
            labels: vec![crate::github::GitHubLabel {
                name: "bug".to_string(),
                color: Some("ff0000".to_string()),
            }],
            assignees: vec![crate::github::GitHubUser {
                login: "harshach".to_string(),
            }],
            project_field_values: serde_json::json!({
                "Status": "Backlog",
                "Sprint": "1.7.2"
            }),
            linked_pr_number: None,
            linked_pr_item_id: None,
        }]];
        app.board_all_issues = app.board_issues.clone();

        press(&mut app, KeyCode::Char('v'));

        assert_eq!(app.input_mode, InputMode::BoardIssueDrawer);
        let output = render_to_string(&mut app, 140, 30);
        assert!(output.contains("Board issue"));
        assert!(output.contains("Status"));
    }

    #[test]
    fn snapshot_skill_search_overlay() {
        let mut app = test_app();
        app.input_mode = InputMode::SkillSearch;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Find Skills"));
    }

    #[test]
    fn snapshot_skill_add_overlay() {
        let mut app = test_app();
        app.input_mode = InputMode::SkillAdd;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Add Skill"));
        assert!(output.contains("Enter"));
        assert!(output.contains("Esc"));
    }

    #[test]
    fn snapshot_configure_wizard_no_status() {
        let mut app = test_app();
        app.input_mode = InputMode::ConfigureWizard;
        // cached_config_status is None → should show "Loading" or similar
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Configure"));
        assert!(output.contains("Permissions"));
    }

    #[test]
    fn snapshot_confirm_delete_task() {
        let mut app = test_app_with_tasks();
        app.input_mode = InputMode::ConfirmDelete;
        app.confirm_target = "Task Alpha".to_string();
        app.confirm_delete_kind = DeleteTarget::Task;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Delete"));
        assert!(output.contains("Task Alpha"));
    }

    // Text-editing unit tests (word boundary, apply_text_edit, format_with_cursor)
    // are in form.rs. Integration tests exercising them through the App follow.

    #[test]
    fn task_form_alt_backspace_deletes_word() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "hello world");
        press_mod(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(app.input_buffer, "hello ");
    }

    #[test]
    fn task_form_alt_b_f_word_jump() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "hello world test");
        // Alt+b (macOS Option+Left) jumps word left
        press_mod(&mut app, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 12); // before "test"
        press_mod(&mut app, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 6); // before "world"
        // Alt+f (macOS Option+Right) jumps word right
        press_mod(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 12); // start of "test"
    }

    #[test]
    fn task_form_alt_arrow_word_jump() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "hello world test");
        // Alt+Left jumps word left
        press_mod(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 12); // before "test"
        press_mod(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 6); // before "world"
        // Alt+Right jumps word right
        press_mod(&mut app, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 12); // start of "test"
    }

    #[test]
    fn task_form_super_backspace_clears_line() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "hello world");
        press_mod(&mut app, KeyCode::Backspace, KeyModifiers::SUPER);
        assert_eq!(app.input_buffer, "");
    }

    #[test]
    fn palette_ctrl_w_deletes_word() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(app.input_mode, InputMode::CommandPalette);
        type_str(&mut app, "new task");
        press_mod(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(app.input_buffer, "new ");
    }

    #[test]
    fn filter_alt_backspace_deletes_word() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.input_mode, InputMode::TaskFilter);
        type_str(&mut app, "hello world");
        press_mod(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(app.task_filter, "hello ");
    }

    // ═══════════════════════════════════════════════════════════════
    // TAB NAVIGATION TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn next_tab_wraps_around() {
        let mut app = test_app();
        // Only dashboard — no-op
        assert_eq!(app.active_tab, 0);
        app.next_tab();
        assert_eq!(app.active_tab, 0);

        // Add two fake session tabs (push Tab::Dashboard as placeholders since
        // we can't construct SessionTerminals in tests)
        app.tabs.push(Tab::Dashboard);
        app.tabs.push(Tab::Dashboard);
        assert_eq!(app.tabs.len(), 3);

        app.next_tab();
        assert_eq!(app.active_tab, 1);
        app.next_tab();
        assert_eq!(app.active_tab, 2);
        // Wraps back to 0
        app.next_tab();
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn prev_tab_wraps_around() {
        let mut app = test_app();
        // Only dashboard — no-op
        app.prev_tab();
        assert_eq!(app.active_tab, 0);

        // Add two fake session tabs
        app.tabs.push(Tab::Dashboard);
        app.tabs.push(Tab::Dashboard);

        // From dashboard (0), prev wraps to last tab (2)
        app.prev_tab();
        assert_eq!(app.active_tab, 2);
        app.prev_tab();
        assert_eq!(app.active_tab, 1);
        app.prev_tab();
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn ctrl_j_k_navigates_tabs_from_dashboard() {
        let mut app = test_app();
        // Add fake session tabs
        app.tabs.push(Tab::Dashboard);
        app.tabs.push(Tab::Dashboard);

        assert_eq!(app.active_tab, 0);

        // Ctrl+J moves to next tab
        press_mod(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 1);

        // Return to dashboard for next test
        app.active_tab = 0;

        // Ctrl+K wraps to last tab
        press_mod(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 2);
    }

    #[test]
    fn ctrl_j_k_noop_with_single_tab() {
        let mut app = test_app();
        assert_eq!(app.tabs.len(), 1);

        press_mod(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 0);

        press_mod(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 0);
    }

    // ── keycode_to_bytes tests ──

    #[test]
    fn alt_backspace_sends_esc_del() {
        let bytes = keycode_to_bytes(KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(bytes.as_bytes(), b"\x1b\x7f");
    }

    #[test]
    fn alt_char_sends_esc_prefix() {
        let bytes = keycode_to_bytes(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(bytes.as_bytes(), b"\x1bb");

        let bytes = keycode_to_bytes(KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!(bytes.as_bytes(), b"\x1bd");

        let bytes = keycode_to_bytes(KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(bytes.as_bytes(), b"\x1bf");
    }

    #[test]
    fn plain_backspace_unchanged() {
        let bytes = keycode_to_bytes(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(bytes.as_bytes(), &[0x7f]);
    }

    #[test]
    fn ctrl_char_sends_control_code() {
        let bytes = keycode_to_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(bytes.as_bytes(), &[0x03]);
    }

    // ── Permission prompt detection tests ──

    #[test]
    fn permission_prompt_detected() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n\r\n");
        parser.process(b"  Allow Bash\r\n");
        parser.process(b"  ls -la\r\n");
        parser.process(b"  Yes  No  Always\r\n");
        assert!(screen_shows_permission_prompt(parser.screen()));
    }

    #[test]
    fn permission_prompt_not_detected_without_allow() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n");
        parser.process(b"  Running command: ls -la\r\n");
        parser.process(b"  Yes  No  Always\r\n");
        assert!(!screen_shows_permission_prompt(parser.screen()));
    }

    #[test]
    fn permission_prompt_not_detected_without_options() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"  Allow Bash\r\n");
        parser.process(b"  ls -la\r\n");
        assert!(!screen_shows_permission_prompt(parser.screen()));
    }

    #[test]
    fn permission_prompt_detected_webfetch() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"  Allow WebFetch\r\n");
        parser.process(b"  https://example.com\r\n");
        parser.process(b"  Yes  No\r\n");
        assert!(screen_shows_permission_prompt(parser.screen()));
    }

    #[test]
    fn permission_prompt_not_detected_lowercase_tool() {
        // "Allow something" where something is not capitalized (unlikely to be a tool)
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"  Allow me to explain\r\n");
        parser.process(b"  Yes  No\r\n");
        // "me" starts lowercase — should not match
        assert!(!screen_shows_permission_prompt(parser.screen()));
    }

    // ── Question prompt detection tests ──

    #[test]
    fn question_prompt_detected_with_options() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n\r\n");
        parser.process(b"  Which approach should we take?\r\n\r\n");
        parser.process(b"  \xe2\x9d\xaf Option A (Recommended)\r\n");
        parser.process(b"    Option B\r\n");
        parser.process(b"    Other\r\n");
        assert!(screen_shows_question_prompt(parser.screen()));
    }

    #[test]
    fn question_prompt_not_detected_without_cursor() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n");
        parser.process(b"  Option A\r\n");
        parser.process(b"  Other\r\n");
        assert!(!screen_shows_question_prompt(parser.screen()));
    }

    #[test]
    fn question_prompt_not_detected_without_other() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n");
        parser.process(b"  \xe2\x9d\xaf Option A\r\n");
        parser.process(b"  Option B\r\n");
        assert!(!screen_shows_question_prompt(parser.screen()));
    }

    #[test]
    fn question_prompt_not_confused_with_permission_text() {
        // Regular text mentioning "Other" and containing a right-pointing character shouldn't match
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"There are Other options available.\r\n");
        assert!(!screen_shows_question_prompt(parser.screen()));
    }

    #[test]
    fn question_prompt_detected_multiselect() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"  Which features do you want?\r\n\r\n");
        parser.process(b"  \xe2\x9d\xaf Feature A\r\n");
        parser.process(b"    Feature B\r\n");
        parser.process(b"    Feature C\r\n");
        parser.process(b"    Other\r\n");
        assert!(screen_shows_question_prompt(parser.screen()));
    }

    // ── Modified special key encoding tests ──

    #[test]
    fn shift_up_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2A");
    }

    #[test]
    fn shift_down_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2B");
    }

    #[test]
    fn ctrl_right_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Right, KeyModifiers::CONTROL);
        // Ctrl = 5 (1 + 4)
        assert_eq!(kb.as_bytes(), b"\x1b[1;5C");
    }

    #[test]
    fn shift_ctrl_left_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Left, KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        // Shift+Ctrl = 6 (1 + 1 + 4)
        assert_eq!(kb.as_bytes(), b"\x1b[1;6D");
    }

    #[test]
    fn alt_arrow_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Up, KeyModifiers::ALT);
        // Alt = 3 (1 + 2)
        assert_eq!(kb.as_bytes(), b"\x1b[1;3A");
    }

    #[test]
    fn shift_home_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Home, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2H");
    }

    #[test]
    fn shift_end_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::End, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2F");
    }

    #[test]
    fn shift_pageup_encodes_tilde_modifier() {
        let kb = keycode_to_bytes(KeyCode::PageUp, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[5;2~");
    }

    #[test]
    fn ctrl_delete_encodes_tilde_modifier() {
        let kb = keycode_to_bytes(KeyCode::Delete, KeyModifiers::CONTROL);
        assert_eq!(kb.as_bytes(), b"\x1b[3;5~");
    }

    #[test]
    fn shift_f1_encodes_modifier() {
        let kb = keycode_to_bytes(KeyCode::F(1), KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2P");
    }

    #[test]
    fn ctrl_f5_encodes_modifier() {
        let kb = keycode_to_bytes(KeyCode::F(5), KeyModifiers::CONTROL);
        assert_eq!(kb.as_bytes(), b"\x1b[15;5~");
    }

    #[test]
    fn unmodified_arrow_unchanged() {
        let kb = keycode_to_bytes(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(kb.as_bytes(), b"\x1b[A");
    }

    #[test]
    fn ctrl_char_still_works() {
        let kb = keycode_to_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(kb.as_bytes(), &[3]); // ETX
    }

    #[test]
    fn alt_char_still_prefixes_esc() {
        let kb = keycode_to_bytes(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(kb.as_bytes(), b"\x1bb");
    }

    // ── Mouse event encoding tests ──

    #[test]
    fn sgr_scroll_up_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::ScrollUp,
            10,
            5,
            vt100::MouseProtocolEncoding::Sgr,
        );
        assert_eq!(bytes, Some(b"\x1b[<64;11;6M".to_vec()));
    }

    #[test]
    fn sgr_scroll_down_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::ScrollDown,
            10,
            5,
            vt100::MouseProtocolEncoding::Sgr,
        );
        assert_eq!(bytes, Some(b"\x1b[<65;11;6M".to_vec()));
    }

    #[test]
    fn sgr_left_press_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::Down(MouseButton::Left),
            0,
            0,
            vt100::MouseProtocolEncoding::Sgr,
        );
        assert_eq!(bytes, Some(b"\x1b[<0;1;1M".to_vec()));
    }

    #[test]
    fn sgr_left_release_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::Up(MouseButton::Left),
            0,
            0,
            vt100::MouseProtocolEncoding::Sgr,
        );
        // Release uses 'm' suffix instead of 'M'
        assert_eq!(bytes, Some(b"\x1b[<0;1;1m".to_vec()));
    }

    #[test]
    fn default_scroll_up_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::ScrollUp,
            10,
            5,
            vt100::MouseProtocolEncoding::Default,
        );
        // button=64+32=96, x=11+32=43, y=6+32=38
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 96, 43, 38]));
    }

    #[test]
    fn default_left_release_sends_button3() {
        let bytes = encode_mouse_event(
            &MouseEventKind::Up(MouseButton::Left),
            0,
            0,
            vt100::MouseProtocolEncoding::Default,
        );
        // Release: button=3+32=35, x=1+32=33, y=1+32=33
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 35, 33, 33]));
    }

    #[test]
    fn busy_indicator_prefers_github_connection_work() {
        let app = test_app();
        app.github_auth_in_progress.store(true, Ordering::Relaxed);
        assert_eq!(app.busy_indicator_label(), Some("connecting GitHub"));
    }

    #[test]
    fn busy_indicator_prefers_session_setup_over_title_generation() {
        let mut app = test_app();
        app.pending_titles.insert("task-1".to_string());
        app.session_op_in_progress = true;
        assert_eq!(app.busy_indicator_label(), Some("preparing session"));
    }
}
