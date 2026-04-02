//! CLI entry point for claustre.
//!
//! Parses subcommands via clap and dispatches to the TUI dashboard,
//! session management, autonomous task chains, or skill operations.

use claustre::{
    config, configure, github, github_app, runtime, session, session_host, session_update, skills,
    store, sync, threads, tui, update, workflows,
};

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

fn open_store() -> Result<store::Store> {
    config::ensure_dirs()?;
    let store = store::Store::open()?;
    store.migrate()?;
    Ok(store)
}

#[derive(Parser)]
#[command(
    name = "claustre",
    about = "Orchestrate multiple Claude Code sessions",
    version = update::VERSION,
    before_help = concat!("claustre ", env!("CLAUSTRE_VERSION")),
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the TUI dashboard (default)
    Dashboard,
    /// Initialize claustre config directory
    Init,
    /// Run the onboarding wizard to configure Claude Code, gh, and permissions
    Configure,
    /// Add a project to claustre
    AddProject {
        /// Display name for the project
        name: String,
        /// Path to the git repository
        #[arg(default_value = ".")]
        path: String,
    },
    /// Add a task to a project
    AddTask {
        /// Project name
        project: String,
        /// Task title
        title: String,
        /// Task description
        #[arg(short, long, default_value = "")]
        description: String,
        /// Task mode: autonomous or supervised
        #[arg(short, long, default_value = "supervised")]
        mode: String,
    },
    /// List projects
    ListProjects,
    /// List tasks for a project
    ListTasks {
        /// Project name
        project: String,
    },
    /// Show stats for a project
    Stats {
        /// Project name
        project: String,
    },
    /// Remove a project from claustre
    RemoveProject {
        /// Project name
        project: String,
    },
    /// Export tasks for a project to .claustre/tasks.json in the project repo
    Export {
        /// Project name
        project: String,
        /// Output path (default: `<repo>/.claustre/tasks.json`)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Manage agent skills (skills.sh integration)
    Skills {
        #[command(subcommand)]
        action: Option<SkillsAction>,
    },
    /// Run autonomous task chain for a session (blocking loop)
    FeedNext {
        /// Session ID to feed tasks to
        #[arg(long)]
        session_id: String,
        /// Launch Claude with --remote
        #[arg(long)]
        remote: bool,
        /// Claude model to use (e.g. claude-opus-4-6, claude-sonnet-4-6)
        #[arg(long, default_value = "claude-opus-4-6")]
        model: String,
        /// Reasoning effort level (min, low, medium, high, max)
        #[arg(long, default_value = "max")]
        effort: String,
    },
    /// Update session state from hooks (transition task on PR, resume, etc.)
    SessionUpdate {
        /// Session ID to update
        #[arg(long)]
        session_id: String,
        /// PR URL — if provided, transitions the in-progress task to `in_review`
        #[arg(long)]
        pr_url: Option<String>,
        /// Cumulative input tokens from this session's conversation
        #[arg(long)]
        input_tokens: Option<i64>,
        /// Cumulative output tokens from this session's conversation
        #[arg(long)]
        output_tokens: Option<i64>,
        /// Signal that the user resumed interaction — transitions `in_review` back to working
        #[arg(long)]
        resumed: bool,
        /// Claude CLI's internal session ID (for --resume support)
        #[arg(long)]
        claude_session_id: Option<String>,
        /// Force session to idle (used by the Notification hook on `idle_prompt`)
        #[arg(long)]
        set_idle: bool,
    },
    /// Run a session host (PTY owner + socket server, detached from TUI)
    SessionHost {
        /// Session ID
        #[arg(long)]
        session_id: String,
        /// Working directory (worktree path)
        #[arg(long)]
        worktree_path: String,
        /// Command to run in the PTY (everything after --)
        #[arg(last = true)]
        cmd: Vec<String>,
    },
    /// Monitor PR comments and implement valid review feedback in a loop
    ReviewLoop {
        /// Session ID whose task's PR to monitor
        #[arg(long)]
        session_id: String,
    },
    /// Sync claustre state across machines via a git repo
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },
    /// GitHub cache/auth/status operations
    #[command(name = "github")]
    GitHub {
        #[command(subcommand)]
        action: GitHubAction,
    },
    /// Thread launch, focus, provider switching, and clipboard capture
    Thread {
        #[command(subcommand)]
        action: ThreadAction,
    },
    /// Workflow loading and run control
    Workflow {
        #[command(subcommand)]
        action: WorkflowAction,
    },
    /// Runtime build/service control for a launched thread
    Runtime {
        #[command(subcommand)]
        action: RuntimeAction,
    },
    /// Open a thread from a notification or shell integration target
    Open {
        /// Thread ID to open/focus
        #[arg(long)]
        thread: String,
    },
    /// Print shell integration script (add `eval "$(claustre shell-init)"` to your .zshrc/.bashrc)
    ShellInit,
    /// Verify the binary is functional (used by auto-update smoke test)
    HealthCheck,
    /// Update claustre to the latest version
    Update,
    /// Roll back to the previous binary version after a bad auto-update
    Rollback,
}

#[derive(Subcommand)]
enum SyncAction {
    /// Initialize the sync git repo (~/.claustre/sync/)
    Init {
        /// Remote URL to clone from (leave empty for local-only)
        url: Option<String>,
    },
    /// Export local state and push to the sync repo
    Push,
    /// Pull from the sync repo and import state
    Pull,
    /// Print the sync directory path (with shell-init, `claustre sync cd` changes directory directly)
    Cd,
}

#[derive(Subcommand)]
enum SkillsAction {
    /// Search for skills on skills.sh
    Find {
        /// Search query
        query: String,
    },
    /// Add a skill package
    Add {
        /// Package (e.g. owner/repo or owner/repo@skill)
        package: String,
        /// Install to a project instead of globally
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Remove an installed skill
    Remove {
        /// Skill name to remove
        name: String,
        /// Remove from project instead of global
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Update all installed skills
    Update,
}

#[derive(Subcommand)]
enum GitHubAction {
    /// Show GitHub auth status and local cache counts
    Status,
    /// Run `gh auth login` as the current fallback auth flow
    Login,
    /// Sync one or all linked repos into the local GitHub cache
    Sync {
        /// Optional project name to sync. Defaults to all git-linked projects.
        #[arg(long)]
        project: Option<String>,
    },
    /// Run `gh auth logout`
    Disconnect,
}

#[derive(Subcommand)]
enum ThreadAction {
    /// Create a thread workspace from an existing task
    Launch {
        /// Task ID to launch a thread for
        #[arg(long)]
        task_id: String,
        /// Provider to use for the thread (`claude`, `codex`, `gemini`, `local`)
        #[arg(long)]
        provider: Option<String>,
        /// Provider profile name
        #[arg(long)]
        profile: Option<String>,
        /// Runtime profile from sandbox.yaml
        #[arg(long)]
        runtime_profile: Option<String>,
        /// Optional workflow to attach immediately
        #[arg(long)]
        workflow: Option<String>,
    },
    /// Show the stored metadata for a thread
    Focus {
        /// Thread ID
        #[arg(long)]
        thread_id: String,
    },
    /// Change the active provider for a thread
    SwitchProvider {
        /// Thread ID
        #[arg(long)]
        thread_id: String,
        /// Provider kind
        #[arg(long)]
        provider: String,
        /// Provider profile
        #[arg(long)]
        profile: Option<String>,
    },
    /// Capture text/image content from the system clipboard into a thread
    PasteClipboard {
        /// Thread ID
        #[arg(long)]
        thread_id: String,
        /// Optional draft key to associate with the pending attachment
        #[arg(long)]
        draft_key: Option<String>,
    },
}

#[derive(Subcommand)]
enum WorkflowAction {
    /// List available workflow definitions
    List {
        /// Optional project name to include repo-local workflows
        #[arg(long)]
        project: Option<String>,
    },
    /// Start a workflow run for a thread
    Run {
        /// Workflow name
        #[arg(long)]
        name: String,
        /// Thread ID
        #[arg(long)]
        thread_id: String,
    },
    /// Advance a workflow run to the next stage
    Resume {
        /// Workflow run ID
        #[arg(long)]
        run_id: String,
    },
    /// Approve a waiting stage and advance the workflow
    Approve {
        /// Workflow run ID
        #[arg(long)]
        run_id: String,
        /// Stage name to approve
        #[arg(long)]
        stage: String,
    },
    /// Show workflow artifacts for a run
    Artifact {
        /// Workflow run ID
        #[arg(long)]
        run_id: String,
    },
}

#[derive(Subcommand)]
enum RuntimeAction {
    /// Build and start runtime services for a thread
    Up {
        /// Thread ID
        #[arg(long)]
        thread_id: String,
        /// Override runtime profile
        #[arg(long)]
        profile: Option<String>,
    },
    /// Stop runtime services for a thread
    Down {
        /// Thread ID
        #[arg(long)]
        thread_id: String,
    },
    /// Restart runtime services for a thread
    Restart {
        /// Thread ID
        #[arg(long)]
        thread_id: String,
    },
    /// Re-run runtime health checks
    Health {
        /// Thread ID
        #[arg(long)]
        thread_id: String,
    },
    /// Print the log path for a runtime service
    Logs {
        /// Thread ID
        #[arg(long)]
        thread_id: String,
        /// Service name
        #[arg(long)]
        service: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Dashboard) {
        Commands::Init => {
            config::ensure_dirs()?;
            println!("claustre initialized at ~/.claustre/");
            Ok(())
        }
        Commands::Configure => configure::run(),
        Commands::AddProject { name, path } => {
            anyhow::ensure!(!name.trim().is_empty(), "project name must not be empty");
            let store = open_store()?;
            let abs_path =
                std::fs::canonicalize(&path).with_context(|| format!("invalid path: {path}"))?;
            let abs_str = abs_path.to_str().context("path contains invalid UTF-8")?;
            let default_branch = config::detect_default_branch(abs_str);
            let project = store.create_project(&name, abs_str, &default_branch, true)?;
            println!(
                "Added project '{}' ({}) [branch: {}]",
                project.name, project.repo_path, project.default_branch
            );
            Ok(())
        }
        Commands::AddTask {
            project,
            title,
            description,
            mode,
        } => {
            anyhow::ensure!(!title.trim().is_empty(), "task title must not be empty");
            let store = open_store()?;
            let proj = find_project_by_name(&store, &project)?;
            let task_mode: store::TaskMode = mode.parse().map_err(|_| {
                anyhow::anyhow!("invalid task mode '{mode}': expected 'autonomous' or 'supervised'")
            })?;
            let task = store.create_task(
                &proj.id,
                &title,
                &description,
                task_mode,
                None,
                None,
                store::PushMode::Pr,
                false,
            )?;
            println!(
                "Created task '{}' ({}) for project '{}'",
                task.title,
                task.mode.as_str(),
                proj.name
            );
            sync::try_auto_push();
            Ok(())
        }
        Commands::ListProjects => {
            let store = open_store()?;
            let projects = store.list_projects()?;
            if projects.is_empty() {
                println!("No projects. Use `claustre add-project <name> <path>` to add one.");
            } else {
                for p in &projects {
                    let sessions = store.list_active_sessions_for_project(&p.id)?;
                    let tasks = store.list_tasks_for_project(&p.id)?;
                    let pending = tasks
                        .iter()
                        .filter(|t| t.status == store::TaskStatus::Pending)
                        .count();
                    let in_review = tasks
                        .iter()
                        .filter(|t| t.status == store::TaskStatus::InReview)
                        .count();
                    println!(
                        "  {} — {} sessions, {} pending, {} in review ({})",
                        p.name,
                        sessions.len(),
                        pending,
                        in_review,
                        p.repo_path,
                    );
                }
            }
            Ok(())
        }
        Commands::ListTasks { project } => {
            let store = open_store()?;
            let proj = find_project_by_name(&store, &project)?;
            let tasks = store.list_tasks_for_project(&proj.id)?;
            if tasks.is_empty() {
                println!("No tasks for '{}'.", proj.name);
            } else {
                for t in &tasks {
                    println!(
                        "  {} {} [{}] ({})",
                        t.status.symbol(),
                        t.title,
                        t.status.as_str(),
                        t.mode.as_str(),
                    );
                }
            }
            Ok(())
        }
        Commands::Stats { project } => {
            let store = open_store()?;
            let proj = find_project_by_name(&store, &project)?;
            let stats = store.project_stats(&proj.id)?;
            println!("Stats for '{}':", proj.name);
            println!("  Total tasks:     {}", stats.total_tasks);
            println!("  Completed:       {}", stats.completed_tasks);
            println!("  Sessions run:    {}", stats.total_sessions);
            println!("  Total time:      {}", stats.formatted_time());
            println!("  Tokens used:     {}", stats.total_tokens());
            println!("  Avg task time:   {}", stats.formatted_avg_task_time());
            Ok(())
        }
        Commands::RemoveProject { project } => {
            let store = open_store()?;
            let proj = find_project_by_name(&store, &project)?;
            store.delete_project(&proj.id)?;
            println!("Removed project '{}'", proj.name);
            Ok(())
        }
        Commands::Export { project, output } => {
            let store = open_store()?;
            let proj = find_project_by_name(&store, &project)?;
            let tasks = store.list_tasks_for_project(&proj.id)?;
            let stats = store.project_stats(&proj.id)?;

            let export = serde_json::json!({
                "project": proj.name,
                "repo_path": proj.repo_path,
                "exported_at": chrono::Utc::now().to_rfc3339(),
                "stats": {
                    "total_tasks": stats.total_tasks,
                    "completed_tasks": stats.completed_tasks,
                    "total_sessions": stats.total_sessions,
                    "total_time": stats.formatted_time(),
                    "total_tokens": stats.total_tokens(),
                },
                "tasks": tasks,
            });

            let json = serde_json::to_string_pretty(&export)?;

            let output_path = if let Some(ref out) = output {
                std::path::PathBuf::from(out)
            } else {
                let claustre_dir = Path::new(&proj.repo_path).join(".claustre");
                fs::create_dir_all(&claustre_dir)?;
                claustre_dir.join("tasks.json")
            };

            fs::write(&output_path, &json)?;
            println!(
                "Exported {} tasks to {}",
                tasks.len(),
                output_path.display()
            );
            Ok(())
        }
        Commands::Skills { action } => match action {
            None => {
                println!("Global skills:");
                let global = skills::list_skills(true, None)?;
                if global.is_empty() {
                    println!("  (none)");
                } else {
                    for s in &global {
                        println!("  {} — {}", s.name, s.path);
                        if !s.agents.is_empty() {
                            println!("    Agents: {}", s.agents.join(", "));
                        }
                    }
                }
                Ok(())
            }
            Some(SkillsAction::Find { query }) => {
                let results = skills::find_skills(&query)?;
                if results.is_empty() {
                    println!("No skills found for '{query}'");
                } else {
                    for r in &results {
                        println!("  {} — {}", r.package, r.url);
                    }
                }
                Ok(())
            }
            Some(SkillsAction::Add { package, project }) => {
                let (global, project_path) = if let Some(ref proj_name) = project {
                    let store = open_store()?;
                    let proj = find_project_by_name(&store, proj_name)?;
                    (false, Some(proj.repo_path))
                } else {
                    (true, None)
                };

                let msg = skills::add_skill(&package, global, project_path.as_deref())?;
                println!("{msg}");
                Ok(())
            }
            Some(SkillsAction::Remove { name, project }) => {
                let (global, project_path) = if let Some(ref proj_name) = project {
                    let store = open_store()?;
                    let proj = find_project_by_name(&store, proj_name)?;
                    (false, Some(proj.repo_path))
                } else {
                    (true, None)
                };

                let msg = skills::remove_skill(&name, global, project_path.as_deref())?;
                println!("{msg}");
                Ok(())
            }
            Some(SkillsAction::Update) => {
                let msg = skills::update_skills()?;
                println!("{msg}");
                Ok(())
            }
        },
        Commands::Sync { action } => match action {
            SyncAction::Init { url } => sync::init(url.as_deref()),
            SyncAction::Push => {
                let store = open_store()?;
                sync::push(&store)
            }
            SyncAction::Pull => {
                let store = open_store()?;
                sync::pull(&store)
            }
            SyncAction::Cd => {
                let sync_dir = config::sync_dir()?;
                print!("{}", sync_dir.display());
                Ok(())
            }
        },
        Commands::GitHub { action } => run_github_command(action),
        Commands::Thread { action } => run_thread_command(action),
        Commands::Workflow { action } => run_workflow_command(action),
        Commands::Runtime { action } => run_runtime_command(action),
        Commands::Open { thread } => {
            let store = open_store()?;
            let thread = threads::focus_thread(&store, &thread)?;
            println!("{} {}", thread.id, thread.title);
            if let Some(worktree_path) = thread.worktree_path.as_deref() {
                println!("worktree: {worktree_path}");
            }
            if let Some(session_id) = thread.session_id.as_deref() {
                println!("session: {session_id}");
            }
            Ok(())
        }
        Commands::FeedNext {
            session_id,
            remote,
            model,
            effort,
        } => run_feed_next(&session_id, remote, &model, &effort),
        Commands::SessionUpdate {
            session_id,
            pr_url,
            input_tokens,
            output_tokens,
            resumed,
            claude_session_id,
            set_idle,
        } => {
            let store = open_store()?;

            // Read Claude's task progress from tmp file (if it exists)
            let progress = if let Ok(progress_path) = config::session_progress_file(&session_id)
                && progress_path.exists()
                && let Ok(content) = fs::read_to_string(&progress_path)
                && let Ok(items) = serde_json::from_str::<Vec<store::ClaudeProgressItem>>(&content)
            {
                Some(items)
            } else {
                None
            };

            let outcome = session_update::apply(
                &store,
                &session_update::SessionUpdateArgs {
                    session_id: &session_id,
                    pr_url: pr_url.as_deref(),
                    input_tokens,
                    output_tokens,
                    resumed,
                    claude_session_id: claude_session_id.as_deref(),
                    progress,
                    set_idle,
                },
            )?;

            // Fire notification for new PRs
            if let session_update::SessionUpdateOutcome::PrDetected {
                ref task_id,
                is_new_pr: true,
            } = outcome
                && let Some(ref url) = pr_url
            {
                let cfg = config::load()?;
                if cfg.notifications.enabled {
                    let task = store.get_task(task_id)?;
                    cfg.notifications.notify(&task.title, Some(url));
                }
            }

            // Auto sync push on task state changes (fire-and-forget)
            sync::try_auto_push();

            Ok(())
        }
        Commands::SessionHost {
            session_id,
            worktree_path,
            cmd,
        } => session_host::run(&session_id, &cmd, &worktree_path),
        Commands::ReviewLoop { session_id } => run_review_loop(&session_id),
        Commands::ShellInit => {
            print!("{}", include_str!("shell_init.sh"));
            Ok(())
        }
        Commands::HealthCheck => {
            let store = open_store()?;
            store.health_check()?;
            println!("ok {}", update::VERSION);
            Ok(())
        }
        Commands::Update => update::run_update(),
        Commands::Rollback => update::rollback(),
        Commands::Dashboard => {
            // Auto-update before opening TUI (if configured)
            let cfg = config::load().unwrap_or_default();
            if cfg.auto_update {
                match update::check_and_update() {
                    update::UpdateCheckResult::Updated { new_version } => {
                        eprintln!("Updated to {new_version}, restarting...");
                        // Re-exec the new binary so the TUI starts with the fresh version
                        let exe =
                            std::env::current_exe().context("could not determine executable")?;
                        let args: Vec<String> = std::env::args().collect();
                        let err = exec_process(&exe, &args);
                        // exec_process only returns on error
                        anyhow::bail!("failed to re-exec after update: {err}");
                    }
                    update::UpdateCheckResult::Available {
                        new_version,
                        reason,
                    } => {
                        eprintln!("Update to {new_version} available but install failed: {reason}");
                    }
                    update::UpdateCheckResult::UpToDate
                    | update::UpdateCheckResult::Failed { .. } => {}
                }
            }

            let store = open_store()?;

            // Clean up socket/PID files from crashed session-hosts
            let _ = config::cleanup_stale_sockets();

            // Install panic hook to restore terminal on panics
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::event::DisableMouseCapture,
                    crossterm::event::DisableBracketedPaste,
                );
                ratatui::restore();
                default_hook(info);
            }));

            // Run TUI (blocking)
            tui::run(store)
        }
    }
}

fn run_github_command(action: GitHubAction) -> Result<()> {
    match action {
        GitHubAction::Status => {
            let store = open_store()?;
            let cfg = config::load()?;
            let status = github_app::local_status(&cfg.github_app)?;
            println!(
                "GitHub App available: {}",
                if status.configured { "yes" } else { "no" }
            );
            println!(
                "GitHub App source: {}",
                if status.custom_app_configured {
                    "custom config"
                } else if status.configured {
                    "bundled defaults"
                } else {
                    "not available"
                }
            );
            println!(
                "GitHub App client_id: {}",
                status.client_id.as_deref().unwrap_or("not set")
            );
            println!(
                "GitHub App user: {}",
                status.user_login.as_deref().unwrap_or("not authenticated")
            );
            println!(
                "GitHub App token expires: {}",
                status.expires_at.as_deref().unwrap_or("unknown")
            );
            println!(
                "GitHub App installation default: {}",
                status
                    .default_installation_id
                    .as_deref()
                    .unwrap_or("none configured")
            );
            println!(
                "GitHub App project default: {}",
                status
                    .default_project_id
                    .as_deref()
                    .unwrap_or("none configured")
            );
            println!(
                "GitHub CLI: {}",
                status.gh_user_login.as_deref().map_or_else(
                    || "not authenticated".to_string(),
                    |login| format!("authenticated as {login}"),
                )
            );
            if status.gh_authenticated {
                println!(
                    "GitHub CLI scopes: {}",
                    if status.gh_scopes.is_empty() {
                        "unknown".to_string()
                    } else {
                        status.gh_scopes.join(", ")
                    }
                );
                println!(
                    "Projects v2 access: {}",
                    if status.gh_has_project_scope {
                        "ready"
                    } else {
                        "missing read:project"
                    }
                );
            }
            if let Some(token) = github_app::ensure_access_token(&cfg.github_app)? {
                if let Ok(installations) = github_app::list_installations(&token) {
                    println!("GitHub App installations: {}", installations.len());
                    for installation in installations.iter().take(5) {
                        println!("  - {} ({})", installation.account.login, installation.id);
                    }
                }
            } else if let Ok(status) = github::auth_status() {
                println!("gh fallback: {status}");
            }

            let repos = store.list_github_repos()?;
            let item_count = repos
                .iter()
                .map(|repo| {
                    store
                        .list_github_items_for_repo(&repo.id)
                        .map(|items| items.len())
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .sum::<usize>();
            println!("cached repos: {}", repos.len());
            println!("cached items: {item_count}");
            Ok(())
        }
        GitHubAction::Login => {
            let cfg = config::load()?;
            if github_app::app_auth_available(&cfg.github_app) {
                println!("Starting GitHub App device flow...");
                github_app::authenticate_device_flow(&cfg.github_app, |event| match event {
                    github_app::GitHubAuthMessage::Prompt(prompt) => {
                        println!(
                            "Open: {}",
                            prompt
                                .verification_uri_complete
                                .as_deref()
                                .unwrap_or(&prompt.verification_uri)
                        );
                        println!("Code: {}", prompt.user_code);
                        println!("Expires: {}", prompt.expires_at);
                    }
                    github_app::GitHubAuthMessage::Success(session) => {
                        println!(
                            "Authenticated as {}",
                            session.user_login.as_deref().unwrap_or("unknown user")
                        );
                    }
                    github_app::GitHubAuthMessage::CliSuccess(login) => {
                        println!(
                            "Connected to GitHub CLI as {}",
                            login.as_deref().unwrap_or("unknown user")
                        );
                    }
                    github_app::GitHubAuthMessage::Failed(error) => {
                        eprintln!("GitHub App auth failed: {error}");
                    }
                    github_app::GitHubAuthMessage::Disconnected => {}
                })?;
            } else {
                println!("Starting GitHub CLI browser login...");
                let login = github_app::github_cli_connect()?;
                println!(
                    "Connected to GitHub CLI as {}",
                    login.as_deref().unwrap_or("unknown user")
                );
            }
            Ok(())
        }
        GitHubAction::Sync { project } => {
            let store = open_store()?;
            let projects = if let Some(project_name) = project {
                vec![find_project_by_name(&store, &project_name)?]
            } else {
                store
                    .list_projects()?
                    .into_iter()
                    .filter(|project| project.is_git_linked)
                    .collect()
            };
            anyhow::ensure!(!projects.is_empty(), "no git-linked projects to sync");
            for project in projects {
                let summary = github::sync_project_repo(&store, &project)?;
                println!(
                    "{}: {} issues, {} PRs, {} comments, {} reviews, {} review comments",
                    summary.repo_full_name,
                    summary.issues_synced,
                    summary.prs_synced,
                    summary.comments_synced,
                    summary.reviews_synced,
                    summary.review_comments_synced
                );
            }
            Ok(())
        }
        GitHubAction::Disconnect => {
            let cfg = config::load()?;
            if github_app::app_auth_available(&cfg.github_app)
                && github_app::load_session()?.is_some()
            {
                github_app::clear_session()?;
                println!("Cleared GitHub App session");
            } else {
                github_app::github_cli_disconnect()?;
                println!("Cleared GitHub CLI session");
            }
            Ok(())
        }
    }
}

fn run_thread_command(action: ThreadAction) -> Result<()> {
    match action {
        ThreadAction::Launch {
            task_id,
            provider,
            profile,
            runtime_profile,
            workflow,
        } => {
            let store = open_store()?;
            let cfg = config::load()?;
            let provider_kind = provider.as_deref().map(parse_provider_kind).transpose()?;
            let result = threads::launch_thread(
                &store,
                &cfg,
                &threads::LaunchThreadArgs {
                    task_id,
                    provider_kind,
                    provider_profile: profile,
                    runtime_profile,
                    workflow_name: workflow,
                    thread_title: None,
                    initial_prompt: None,
                },
            )?;
            println!("thread: {} {}", result.thread.id, result.thread.title);
            if let Some(worktree_path) = result.thread.worktree_path.as_deref() {
                println!("worktree: {worktree_path}");
            }
            if let Some(bundle) = result.workflow {
                println!("workflow: {} ({})", bundle.run.id, bundle.definition.name);
            }
            Ok(())
        }
        ThreadAction::Focus { thread_id } => {
            let store = open_store()?;
            let thread = threads::focus_thread(&store, &thread_id)?;
            let messages = store.list_thread_messages(&thread.id)?;
            let runs = store.list_thread_runs(&thread.id)?;
            println!("thread: {} {}", thread.id, thread.title);
            println!("status: {}", thread.status);
            println!("provider: {}", thread.provider_kind);
            if let Some(worktree_path) = thread.worktree_path.as_deref() {
                println!("worktree: {worktree_path}");
            }
            println!("messages: {}", messages.len());
            println!("runs: {}", runs.len());
            Ok(())
        }
        ThreadAction::SwitchProvider {
            thread_id,
            provider,
            profile,
        } => {
            let store = open_store()?;
            threads::switch_thread_provider(
                &store,
                &thread_id,
                parse_provider_kind(&provider)?,
                profile.as_deref(),
            )?;
            println!("updated provider for thread {thread_id}");
            Ok(())
        }
        ThreadAction::PasteClipboard {
            thread_id,
            draft_key,
        } => {
            let store = open_store()?;
            match threads::smart_paste_clipboard(&store, &thread_id, draft_key.as_deref())? {
                threads::SmartPasteResult::Text(text) => println!("{text}"),
                threads::SmartPasteResult::Image(attachment) => {
                    println!("{}", attachment.local_path);
                }
            }
            Ok(())
        }
    }
}

fn run_workflow_command(action: WorkflowAction) -> Result<()> {
    match action {
        WorkflowAction::List { project } => {
            let store = open_store()?;
            let repo_root = if let Some(project_name) = project {
                let project = find_project_by_name(&store, &project_name)?;
                Some(PathBuf::from(project.repo_path))
            } else {
                None
            };
            let defs = workflows::sync_workflow_definitions(&store, repo_root.as_deref())?;
            for (definition, stored) in defs {
                println!(
                    "{} [{}] {}",
                    definition.name,
                    stored.scope,
                    definition.description.unwrap_or_default()
                );
            }
            Ok(())
        }
        WorkflowAction::Run { name, thread_id } => {
            let store = open_store()?;
            let thread = store.get_thread(&thread_id)?;
            let project = store.get_project(&thread.project_id)?;
            let bundle = workflows::start_workflow_run(
                &store,
                &name,
                Some(&thread.id),
                thread.github_item_id.as_deref(),
                Some(Path::new(&project.repo_path)),
            )?;
            store.attach_workflow_run_to_thread(&thread.id, Some(&bundle.run.id))?;
            println!("workflow run: {}", bundle.run.id);
            for stage in bundle.stages {
                println!("  {} [{}]", stage.stage_name, stage.status);
            }
            Ok(())
        }
        WorkflowAction::Resume { run_id } => {
            let store = open_store()?;
            let bundle = workflows::resume_workflow_run(&store, &run_id)?;
            println!("workflow run: {} [{}]", bundle.run.id, bundle.run.status);
            for stage in bundle.stages {
                println!("  {} [{}]", stage.stage_name, stage.status);
            }
            Ok(())
        }
        WorkflowAction::Approve { run_id, stage } => {
            let store = open_store()?;
            let bundle = workflows::approve_workflow_stage(&store, &run_id, &stage)?;
            println!("workflow run: {} [{}]", bundle.run.id, bundle.run.status);
            for stage in bundle.stages {
                println!("  {} [{}]", stage.stage_name, stage.status);
            }
            Ok(())
        }
        WorkflowAction::Artifact { run_id } => {
            let store = open_store()?;
            for artifact in store.list_workflow_artifacts(&run_id)? {
                if let Some(local_path) = artifact.local_path.as_deref() {
                    println!(
                        "{} ({}) {}",
                        artifact.artifact_name, artifact.artifact_type, local_path
                    );
                } else {
                    println!("{} ({})", artifact.artifact_name, artifact.artifact_type);
                    if let Some(content_text) = artifact.content_text.as_deref() {
                        println!("{content_text}");
                    }
                }
            }
            Ok(())
        }
    }
}

fn run_runtime_command(action: RuntimeAction) -> Result<()> {
    match action {
        RuntimeAction::Up { thread_id, profile } => {
            let (store, cfg, thread, project, worktree_path) = thread_runtime_context(&thread_id)?;
            if profile.is_some() {
                store.update_thread_session_and_worktree(
                    &thread.id,
                    thread.session_id.as_deref(),
                    thread.worktree_path.as_deref(),
                    thread.branch_name.as_deref(),
                    profile.as_deref(),
                )?;
            }
            let states = runtime::up_profile(
                &store,
                &thread.id,
                &worktree_path,
                Path::new(&project.repo_path),
                &cfg.runtime,
                profile.as_deref(),
            )?;
            for state in states {
                println!("{} [{}]", state.name, state.status);
            }
            Ok(())
        }
        RuntimeAction::Down { thread_id } => {
            let (store, cfg, thread, project, worktree_path) = thread_runtime_context(&thread_id)?;
            for state in runtime::down_profile(
                &store,
                &thread.id,
                &worktree_path,
                Path::new(&project.repo_path),
                &cfg.runtime,
            )? {
                println!("{} [{}]", state.name, state.status);
            }
            Ok(())
        }
        RuntimeAction::Restart { thread_id } => {
            let (store, cfg, thread, project, worktree_path) = thread_runtime_context(&thread_id)?;
            for state in runtime::restart_profile(
                &store,
                &thread.id,
                &worktree_path,
                Path::new(&project.repo_path),
                &cfg.runtime,
            )? {
                println!("{} [{}]", state.name, state.status);
            }
            Ok(())
        }
        RuntimeAction::Health { thread_id } => {
            let (store, cfg, thread, project, worktree_path) = thread_runtime_context(&thread_id)?;
            for state in runtime::health_check_profile(
                &store,
                &thread.id,
                &worktree_path,
                Path::new(&project.repo_path),
                &cfg.runtime,
            )? {
                println!("{} [{}]", state.name, state.status);
            }
            Ok(())
        }
        RuntimeAction::Logs { thread_id, service } => {
            let store = open_store()?;
            if let Some(log_path) = runtime::service_log_path(&store, &thread_id, &service)? {
                println!("{}", log_path.display());
            } else {
                anyhow::bail!("no log path tracked for service '{service}'");
            }
            Ok(())
        }
    }
}

fn parse_provider_kind(raw: &str) -> Result<store::ProviderKind> {
    raw.parse::<store::ProviderKind>()
        .map_err(|error| anyhow::anyhow!(error))
}

fn thread_runtime_context(
    thread_id: &str,
) -> Result<(
    store::Store,
    config::Config,
    store::Thread,
    store::Project,
    PathBuf,
)> {
    let store = open_store()?;
    let cfg = config::load()?;
    let thread = store.get_thread(thread_id)?;
    let project = store.get_project(&thread.project_id)?;
    let worktree_path = thread
        .worktree_path
        .as_ref()
        .map(PathBuf::from)
        .context("thread has no worktree path")?;
    Ok((store, cfg, thread, project, worktree_path))
}

const RATE_LIMIT_THRESHOLD: f64 = 80.0;

/// Check usage cache for rate limit. Returns true if usage is too high to proceed.
#[expect(
    clippy::similar_names,
    reason = "5h and 7d are distinct domain-specific window labels"
)]
fn is_rate_limited_from_cache() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let cache_path = home.join(".claude/statusline-cache.json");
    let Ok(content) = fs::read_to_string(&cache_path) else {
        return false;
    };
    let Ok(cache) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let pct_5h = cache["data"]["pct5h"].as_f64().unwrap_or(0.0);
    let pct_7d = cache["data"]["pct7d"].as_f64().unwrap_or(0.0);
    pct_5h >= RATE_LIMIT_THRESHOLD || pct_7d >= RATE_LIMIT_THRESHOLD
}

/// Blocking loop that feeds autonomous tasks to a Claude session.
///
/// For each task: builds the prompt (including subtasks if any), runs Claude as a
/// blocking subprocess, then checks whether the Stop hook transitioned the task.
/// Continues to the next autonomous task until none remain or rate limited.
fn run_feed_next(session_id: &str, remote: bool, model: &str, effort: &str) -> Result<()> {
    let store = open_store()?;

    // Look up the project's default branch for PR target instructions
    let session = store.get_session(session_id)?;
    let project = store.get_project(&session.project_id)?;

    loop {
        // Check rate limits from the shared cache
        if is_rate_limited_from_cache() {
            eprintln!("feed-next: rate limited (>=80% usage), stopping");
            break;
        }

        // Find the current or next task to work on
        let task = if let Some(t) = store.working_task_for_session(session_id)? {
            // Resume a working task (e.g. after restart)
            t
        } else if let Some(t) = store.interrupted_task_for_session(session_id)? {
            // Resume an interrupted task (claustre restarted while task was active)
            t
        } else if store.in_review_task_for_session(session_id)?.is_some() {
            // Previous task completed or has conflicts — look for next
            match store.next_pending_task_for_session(session_id)? {
                Some(next) => next,
                None => break,
            }
        } else {
            // No working or in-review task — find next pending
            match store.next_pending_task_for_session(session_id)? {
                Some(next) => next,
                None => break,
            }
        };

        // Mark task working if it's still pending or interrupted
        if task.status == store::TaskStatus::Pending {
            store.assign_task_to_session(&task.id, session_id)?;
            store.update_task_status(&task.id, store::TaskStatus::Working)?;
            store.update_session_status(
                session_id,
                store::ClaudeStatus::Working,
                &format!("Starting: {}", task.title),
            )?;
        } else if task.status == store::TaskStatus::Interrupted {
            store.update_task_status(&task.id, store::TaskStatus::Working)?;
            store.update_session_status(
                session_id,
                store::ClaudeStatus::Working,
                &format!("Resumed: {}", task.title),
            )?;
        }

        // Build prompt: if task has subtasks, concatenate them all into an ordered list
        let subtasks = store.list_subtasks_for_task(&task.id)?;
        // Use the task's base branch for PR targeting if set, otherwise project default
        let effective_base = task
            .base
            .as_deref()
            .filter(|b| !b.is_empty())
            .unwrap_or(&project.default_branch);
        let instructions = session::completion_instructions(effective_base, task.push_mode);
        let prompt = if subtasks.is_empty() {
            format!(
                "{}{}{}",
                task.description,
                session::AUTONOMOUS_SUFFIX,
                instructions
            )
        } else {
            use std::fmt::Write;
            let mut p = format!("# {}\n\n{}\n\n## Steps\n\n", task.title, task.description);
            for (i, st) in subtasks.iter().enumerate() {
                let _ = writeln!(p, "{}. **{}**: {}", i + 1, st.title, st.description);
            }
            p.push_str(session::AUTONOMOUS_SUFFIX);
            p.push_str(&instructions);
            p
        };

        // Run Claude as a blocking subprocess
        // If resuming an interrupted task and we have a Claude session ID, use --resume
        // to continue the exact same conversation instead of starting fresh.
        let is_resuming = matches!(
            task.status,
            store::TaskStatus::Working | store::TaskStatus::Interrupted
        );
        let session_data = store.get_session(session_id)?;
        let use_resume = is_resuming && session_data.claude_session_id.is_some();

        if use_resume {
            eprintln!("feed-next: resuming task '{}'", task.title);
        } else {
            eprintln!("feed-next: running task '{}'", task.title);
        }

        let mut cmd = std::process::Command::new("claude");
        if remote {
            cmd.arg("--remote");
        }
        cmd.args(["--model", model, "--effort", effort]);
        if use_resume {
            cmd.arg("--resume");
            cmd.arg(
                session_data
                    .claude_session_id
                    .as_ref()
                    .expect("checked above"),
            );
        } else {
            cmd.arg(&prompt);
        }
        let status = cmd
            .env("CLAUDE_CODE_TASK_LIST_ID", session_id)
            .env("CLAUSTRE_SESSION", "1")
            .status()
            .context("failed to run claude")?;

        if !status.success() {
            let exit_info = match status.code() {
                Some(code) => format!("exit code {code}"),
                None => "terminated by signal".to_string(),
            };
            eprintln!("feed-next: claude exited with {exit_info}, stopping");
            break;
        }

        // After Claude exits, the Stop hook has already fired.
        // Re-read task from DB to check its state.
        let task = store.get_task(&task.id)?;
        if task.status == store::TaskStatus::Working {
            if task.push_mode == store::PushMode::Push {
                // Push-mode tasks are done once Claude commits and pushes — no PR to wait for
                store.update_task_status(&task.id, store::TaskStatus::Done)?;
                store.update_session_status(
                    session_id,
                    store::ClaudeStatus::Done,
                    &format!("Completed: {}", task.title),
                )?;
            } else {
                // PR-mode: Stop hook didn't find a PR — mark in_review as best-effort fallback
                store.update_task_status(&task.id, store::TaskStatus::InReview)?;
            }
        }

        // Mark subtasks done if the task was completed
        if !subtasks.is_empty() {
            for st in &subtasks {
                if st.status != store::TaskStatus::Done {
                    store.update_subtask_status(&st.id, store::TaskStatus::Done)?;
                }
            }
        }

        // Continue loop — will check for next pending task at top
    }

    eprintln!("feed-next: no more tasks, exiting");
    Ok(())
}

/// The review-loop prompt template. Tells Claude to fetch PR comments,
/// evaluate them adversarially, implement valid ones, and provide a summary.
const REVIEW_LOOP_PROMPT: &str = r#"You are reviewing PR comments on this branch. Follow these steps:

1. Run `gh pr view --json number,url --jq '.number'` to get the PR number.
2. Run `gh api repos/{owner}/{repo}/pulls/{number}/comments --jq '.[] | select(.in_reply_to_id == null) | {id: .id, path: .path, line: .line, body: .body, user: .user.login}'` to fetch review comments. Also run `gh api repos/{owner}/{repo}/pulls/{number}/reviews --jq '.[] | select(.state == "CHANGES_REQUESTED" or .state == "COMMENTED") | {id: .id, body: .body, user: .user.login, state: .state}'` to fetch review-level comments.
   - Derive {owner}/{repo} from `gh repo view --json nameWithOwner --jq .nameWithOwner`
3. For EACH comment, evaluate it adversarially:
   - Is this a valid, actionable code review comment?
   - Reject: nitpicks, pure style preferences without substance, comments that misunderstand the code, comments from bots
   - Accept: bug fixes, logic errors, missing edge cases, security issues, meaningful improvements
4. For each ACCEPTED comment:
   - Implement the requested change
   - Stage and commit with a message referencing the review comment
5. If any changes were made, push: `git push`
6. At the end, print a summary table:

## Review Loop Summary

| Comment | Author | Verdict | Reason |
|---------|--------|---------|--------|
| <brief description> | <user> | Accepted/Rejected | <WHY you accepted or rejected it> |

If there are no comments or no actionable comments, just say "No actionable review comments found."

IMPORTANT: This is an autonomous task. Do NOT ask the user for clarification. Make your best judgment and proceed."#;

/// Run a review loop: periodically check PR comments and implement valid feedback.
fn run_review_loop(session_id: &str) -> Result<()> {
    let store = open_store()?;
    let cfg = config::load()?;
    let poll_interval = std::time::Duration::from_secs(cfg.review_loop.poll_interval_secs);
    let prompt = cfg
        .review_loop
        .prompt
        .as_deref()
        .unwrap_or(REVIEW_LOOP_PROMPT);

    loop {
        // Find the in_review task for this session
        let task = store.in_review_task_for_session(session_id)?;
        let task = match task {
            Some(t) if t.pr_url.is_some() => t,
            Some(_) => {
                eprintln!("review-loop: task has no PR URL yet, waiting...");
                std::thread::sleep(poll_interval);
                continue;
            }
            None => {
                // Check if task is done — if so, exit
                let working = store.working_task_for_session(session_id)?;
                if working.is_some() {
                    eprintln!("review-loop: task still working, waiting...");
                    std::thread::sleep(poll_interval);
                    continue;
                }
                eprintln!("review-loop: no in_review task found, exiting");
                break;
            }
        };

        eprintln!("review-loop: checking PR comments for '{}'", task.title);

        // Run Claude with the review prompt
        let status = std::process::Command::new("claude")
            .args(["--model", &cfg.claude.model, "--effort", &cfg.claude.effort])
            .arg(prompt)
            .env("CLAUSTRE_SESSION", "1")
            .status()
            .context("failed to run claude for review loop")?;

        if !status.success() {
            eprintln!(
                "review-loop: claude exited with status {}, will retry",
                status.code().unwrap_or(-1)
            );
        }

        // Check rate limits
        if is_rate_limited_from_cache() {
            eprintln!("review-loop: rate limited, stopping");
            break;
        }

        // Re-check task status — if it's no longer in_review (e.g. merged), stop
        let task = store.get_task(&task.id)?;
        if task.status == store::TaskStatus::Done {
            eprintln!("review-loop: task is done, exiting");
            break;
        }

        eprintln!(
            "review-loop: sleeping {}s before next check",
            poll_interval.as_secs()
        );
        std::thread::sleep(poll_interval);
    }

    Ok(())
}

/// Replace the current process with a new invocation of the given executable.
/// Uses Unix `execv` — only returns on error.
fn exec_process(exe: &Path, args: &[String]) -> std::io::Error {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_exe = CString::new(exe.as_os_str().as_bytes()).expect("executable path contains null");
    let c_args: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_bytes()).expect("argument contains null"))
        .collect();
    let c_arg_ptrs: Vec<&std::ffi::CStr> = c_args.iter().map(AsRef::as_ref).collect();

    // This replaces the process; it only returns on failure.
    nix_execv(&c_exe, &c_arg_ptrs)
}

/// Thin wrapper around `libc::execv` that returns an `io::Error` on failure.
fn nix_execv(exe: &std::ffi::CStr, args: &[&std::ffi::CStr]) -> std::io::Error {
    let ptrs: Vec<*const libc::c_char> = args
        .iter()
        .map(|a| a.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // SAFETY: `exe` and all `ptrs` entries are valid C strings; the array is null-terminated.
    unsafe {
        libc::execv(exe.as_ptr(), ptrs.as_ptr());
    }
    std::io::Error::last_os_error()
}

fn find_project_by_name(store: &store::Store, name: &str) -> Result<store::Project> {
    let projects = store.list_projects()?;
    projects
        .into_iter()
        .find(|p| p.name == name)
        .with_context(|| format!("project '{name}' not found"))
}
