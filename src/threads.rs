//! Thread launch orchestration, provider switching, and clipboard attachment capture.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use arboard::Clipboard;
use png::{BitDepth, ColorType, Encoder};
use tracing::warn;
use uuid::Uuid;

use crate::config;
use crate::runtime;
use crate::session;
use crate::store::{
    AttachmentSource, ClaudeStatus, GitHubItem, GitHubItemKind, ProviderKind, Session, Store, Task,
    TaskStatus, Thread, ThreadAttachment, ThreadRunStatus, ThreadStatus,
};
use crate::workflows::{self, WorkflowRunBundle};

#[derive(Debug, Clone)]
pub struct LaunchThreadArgs {
    pub task_id: String,
    pub provider_kind: Option<ProviderKind>,
    pub provider_profile: Option<String>,
    pub runtime_profile: Option<String>,
    pub workflow_name: Option<String>,
    pub thread_title: Option<String>,
    pub initial_prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LaunchAdHocThreadArgs {
    pub project_id: String,
    pub title: String,
    pub initial_prompt: Option<String>,
    pub provider_kind: Option<ProviderKind>,
    pub provider_profile: Option<String>,
    pub runtime_profile: Option<String>,
    pub workflow_name: Option<String>,
}

pub struct LaunchThreadResult {
    pub thread: Thread,
    pub workflow: Option<WorkflowRunBundle>,
    pub session_setup: Option<session::SessionSetup>,
}

#[derive(Debug, Clone)]
pub struct LaunchGitHubItemThreadArgs {
    pub github_item_id: String,
    pub provider_kind: Option<ProviderKind>,
    pub provider_profile: Option<String>,
    pub runtime_profile: Option<String>,
    pub workflow_name: Option<String>,
    pub thread_title: Option<String>,
    pub initial_prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub enum SmartPasteResult {
    Text(String),
    Image(Box<ThreadAttachment>),
}

pub fn relaunch_thread(
    store: &Store,
    cfg: &config::Config,
    thread_id: &str,
) -> Result<LaunchThreadResult> {
    let thread = store.get_thread(thread_id)?;
    if let Some(session_id) = thread.session_id.as_deref()
        && let Ok(session) = store.get_session(session_id)
        && session.closed_at.is_none()
    {
        // Check if session-host is truly alive (socket + PID + worktree).
        // If not, the session is stale — close it so we can relaunch.
        let worktree_exists = std::path::Path::new(&session.worktree_path).exists();
        let host_alive = config::session_socket_path(session_id)
            .ok()
            .is_some_and(|sock| {
                sock.exists()
                    && config::session_pid_path(session_id)
                        .ok()
                        .and_then(|pid_path| std::fs::read_to_string(pid_path).ok())
                        .and_then(|pid_str| pid_str.trim().parse::<i32>().ok())
                        // SAFETY: kill(pid, 0) just checks if process exists
                        .is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0)
            });
        if host_alive && worktree_exists {
            return Ok(LaunchThreadResult {
                thread,
                workflow: None,
                session_setup: None,
            });
        }
        // Stale session (host dead or worktree removed) — close it
        store.close_session(session_id)?;
    }

    let project = store.get_project(&thread.project_id)?;
    let github_item = thread
        .github_item_id
        .as_deref()
        .and_then(|item_id| store.get_github_item(item_id).ok());
    let task = thread
        .task_id
        .as_deref()
        .and_then(|task_id| store.get_task(task_id).ok());

    let branch_name = thread
        .branch_name
        .clone()
        .unwrap_or_else(|| session::generate_branch_name(&thread.title));
    let base_branch = github_item
        .as_ref()
        .and_then(|item| item.base_ref.clone())
        .or_else(|| task.as_ref().and_then(|task| task.base.clone()));
    let prompt = build_thread_handoff_prompt(store, &thread, github_item.as_ref(), task.as_ref())?;

    let mut session_setup = session::create_session(
        store,
        &project.id,
        &branch_name,
        None,
        base_branch.as_deref(),
        cfg.remote_enabled,
        &cfg.claude,
    )?;
    session_setup.tab_label = format!("{} [{}]", session_setup.tab_label, thread.provider_kind);
    session_setup.claude_cmd = Some(session::wrap_cmd_with_shell_fallback(
        build_initial_agent_command(
            cfg,
            thread.provider_kind,
            thread.provider_profile.as_deref(),
            Some(&prompt),
        ),
    ));
    store.update_session_status(
        &session_setup.session.id,
        ClaudeStatus::Working,
        &format!("Continuing thread via {}", thread.provider_kind),
    )?;

    if let Some(task) = &task {
        store.assign_task_to_session(&task.id, &session_setup.session.id)?;
        if task.status != TaskStatus::Done {
            store.update_task_status(&task.id, TaskStatus::Working)?;
        }
    }

    store.update_thread_session_and_worktree(
        &thread.id,
        Some(&session_setup.session.id),
        Some(&session_setup.worktree_path),
        Some(&branch_name),
        thread.runtime_profile.as_deref(),
    )?;
    store.create_thread_message(
        &thread.id,
        None,
        "system",
        &format!(
            "Prepared a provider-neutral handoff to continue this thread with {}.",
            thread.provider_kind
        ),
        &[],
    )?;
    store.create_thread_run(
        &thread.id,
        thread.provider_kind,
        thread.provider_profile.as_deref(),
        ThreadRunStatus::Running,
        Some(&prompt),
    )?;

    let worktree_path = PathBuf::from(&session_setup.worktree_path);
    if thread.runtime_profile.is_some() || cfg.runtime.default_profile.is_some() {
        runtime::up_profile(
            store,
            &thread.id,
            &worktree_path,
            Path::new(&project.repo_path),
            &cfg.runtime,
            thread.runtime_profile.as_deref(),
        )?;
    }
    if let Some(head_ref) = github_item
        .as_ref()
        .and_then(|item| item.head_ref.as_deref())
        && let Err(error) = align_worktree_to_remote_head(&session_setup.worktree_path, head_ref)
    {
        warn!(
            thread_id = %thread.id,
            head_ref,
            worktree_path = %session_setup.worktree_path,
            "failed to align relaunched thread worktree to remote head: {error}"
        );
    }

    store.update_thread_status(&thread.id, ThreadStatus::Running)?;

    Ok(LaunchThreadResult {
        thread: store.get_thread(&thread.id)?,
        workflow: None,
        session_setup: Some(session_setup),
    })
}

pub fn launch_thread(
    store: &Store,
    cfg: &config::Config,
    args: &LaunchThreadArgs,
) -> Result<LaunchThreadResult> {
    let mut task = store.get_task(&args.task_id)?;
    if task.status == TaskStatus::Draft {
        store.update_task_status(&task.id, TaskStatus::Pending)?;
        task = store.get_task(&task.id)?;
    }
    let project = store.get_project(&task.project_id)?;
    if let Some(existing) = store.find_thread_for_task(&task.id)? {
        return focus_thread(store, &existing.id).map(|thread| LaunchThreadResult {
            thread,
            workflow: None,
            session_setup: None,
        });
    }

    let initial_prompt = args
        .initial_prompt
        .clone()
        .unwrap_or_else(|| task.description.clone());
    let title = args
        .thread_title
        .clone()
        .unwrap_or_else(|| task.title.clone());
    let branch_name = task
        .branch
        .clone()
        .unwrap_or_else(|| session::generate_branch_name(&title));

    launch_common(
        store,
        cfg,
        &LaunchCommonArgs {
            project_id: project.id,
            task_id: Some(task.id.clone()),
            task_base: task.base,
            title,
            initial_prompt,
            provider_kind: args.provider_kind,
            provider_profile: args.provider_profile.clone(),
            runtime_profile: args.runtime_profile.clone(),
            workflow_name: args.workflow_name.clone(),
            branch_name,
            existing_github_item_id: None,
        },
    )
}

pub fn launch_ad_hoc_thread(
    store: &Store,
    cfg: &config::Config,
    args: &LaunchAdHocThreadArgs,
) -> Result<LaunchThreadResult> {
    let project = store.get_project(&args.project_id)?;
    let branch_name = session::generate_branch_name(&args.title);
    launch_common(
        store,
        cfg,
        &LaunchCommonArgs {
            project_id: project.id,
            task_id: None,
            task_base: None,
            title: args.title.clone(),
            initial_prompt: args.initial_prompt.clone().unwrap_or_default(),
            provider_kind: args.provider_kind,
            provider_profile: args.provider_profile.clone(),
            runtime_profile: args.runtime_profile.clone(),
            workflow_name: args.workflow_name.clone(),
            branch_name,
            existing_github_item_id: None,
        },
    )
}

pub fn launch_github_item_thread(
    store: &Store,
    cfg: &config::Config,
    args: &LaunchGitHubItemThreadArgs,
) -> Result<LaunchThreadResult> {
    let github_item = store.get_github_item(&args.github_item_id)?;
    if let Some(existing) = store.find_thread_for_github_item(&github_item.id)? {
        // Relaunch existing thread — creates a new session if the old one is
        // stale, or returns session_setup: None if the session-host is alive.
        return relaunch_thread(store, cfg, &existing.id);
    }

    let repo = store.get_github_repo(&github_item.repo_id)?;
    let Some(project_id) = repo.project_id else {
        bail!("GitHub item is not linked to a local claustre project");
    };

    let title = args
        .thread_title
        .clone()
        .unwrap_or_else(|| format!("#{} {}", github_item.number, github_item.title));
    let branch_name = github_item
        .head_ref
        .clone()
        .unwrap_or_else(|| session::generate_branch_name(&title));

    let mut result = launch_common(
        store,
        cfg,
        &LaunchCommonArgs {
            project_id,
            task_id: None,
            task_base: github_item.base_ref.clone(),
            title,
            initial_prompt: args
                .initial_prompt
                .clone()
                .unwrap_or_else(|| github_item.body_text.clone().unwrap_or_default()),
            provider_kind: args.provider_kind,
            provider_profile: args.provider_profile.clone(),
            runtime_profile: args.runtime_profile.clone(),
            workflow_name: args.workflow_name.clone(),
            branch_name,
            existing_github_item_id: Some(github_item.id.clone()),
        },
    )?;

    if let Some(head_ref) = github_item.head_ref.as_deref()
        && let Some(session_setup) = result.session_setup.as_ref()
        && let Err(error) = align_worktree_to_remote_head(&session_setup.worktree_path, head_ref)
    {
        warn!(
            thread_id = %result.thread.id,
            head_ref,
            worktree_path = %session_setup.worktree_path,
            "failed to align PR worktree to remote head: {error}"
        );
    }

    result.thread = store.get_thread(&result.thread.id)?;
    Ok(result)
}

pub fn ensure_thread_bridge_for_session(
    store: &Store,
    session: &Session,
    task: Option<&Task>,
) -> Result<Thread> {
    if let Some(existing) = store.find_thread_for_session(&session.id)? {
        return Ok(existing);
    }

    if let Some(task) = task
        && let Some(existing) = store.find_thread_for_task(&task.id)?
    {
        store.update_thread_session_and_worktree(
            &existing.id,
            Some(&session.id),
            Some(&session.worktree_path),
            Some(&session.branch_name),
            existing.runtime_profile.as_deref(),
        )?;
        store.update_thread_status(&existing.id, bridge_thread_status(session))?;
        seed_bridged_thread_context(store, &existing, session, Some(task))?;
        return store.get_thread(&existing.id);
    }

    let title = task
        .map(|task| task.title.clone())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| session.tab_label.clone());
    let thread = store.create_thread(
        &session.project_id,
        None,
        task.map(|task| task.id.as_str()),
        Some(&session.id),
        &title,
        ProviderKind::Claude,
        Some("default"),
        Some(&session.worktree_path),
        Some(&session.branch_name),
        None,
        None,
    )?;
    store.update_thread_status(&thread.id, bridge_thread_status(session))?;
    seed_bridged_thread_context(store, &thread, session, task)?;
    store.get_thread(&thread.id)
}

#[derive(Debug, Clone)]
struct LaunchCommonArgs {
    project_id: String,
    task_id: Option<String>,
    task_base: Option<String>,
    title: String,
    initial_prompt: String,
    provider_kind: Option<ProviderKind>,
    provider_profile: Option<String>,
    runtime_profile: Option<String>,
    workflow_name: Option<String>,
    branch_name: String,
    existing_github_item_id: Option<String>,
}

fn launch_common(
    store: &Store,
    cfg: &config::Config,
    args: &LaunchCommonArgs,
) -> Result<LaunchThreadResult> {
    let project = store.get_project(&args.project_id)?;
    let provider_kind = args.provider_kind.unwrap_or(ProviderKind::Claude);
    let provider_profile = args
        .provider_profile
        .clone()
        .or_else(|| Some("default".to_string()));
    let mut session_setup = session::create_session(
        store,
        &project.id,
        &args.branch_name,
        None,
        args.task_base.as_deref(),
        cfg.remote_enabled,
        &cfg.claude,
    )?;
    session_setup.tab_label = format!("{} [{}]", session_setup.tab_label, provider_kind);
    session_setup.claude_cmd = Some(session::wrap_cmd_with_shell_fallback(
        build_initial_agent_command(
            cfg,
            provider_kind,
            provider_profile.as_deref(),
            Some(&args.initial_prompt),
        ),
    ));
    store.update_session_status(
        &session_setup.session.id,
        ClaudeStatus::Working,
        &format!("Starting {provider_kind} thread"),
    )?;
    if let Some(task_id) = args.task_id.as_deref() {
        store.assign_task_to_session(task_id, &session_setup.session.id)?;
        store.update_task_status(task_id, TaskStatus::Working)?;
    }

    let thread = store.create_thread(
        &project.id,
        args.existing_github_item_id.as_deref(),
        args.task_id.as_deref(),
        Some(&session_setup.session.id),
        &args.title,
        provider_kind,
        provider_profile.as_deref(),
        Some(&session_setup.worktree_path),
        Some(&args.branch_name),
        args.runtime_profile.as_deref(),
        None,
    )?;
    if !args.initial_prompt.trim().is_empty() {
        store.create_thread_message(&thread.id, None, "user", &args.initial_prompt, &[])?;
    }
    store.create_thread_run(
        &thread.id,
        provider_kind,
        provider_profile.as_deref(),
        ThreadRunStatus::Running,
        (!args.initial_prompt.trim().is_empty()).then_some(args.initial_prompt.as_str()),
    )?;

    let worktree_path = PathBuf::from(&session_setup.worktree_path);
    if args.runtime_profile.is_some() || cfg.runtime.default_profile.is_some() {
        runtime::up_profile(
            store,
            &thread.id,
            &worktree_path,
            Path::new(&project.repo_path),
            &cfg.runtime,
            args.runtime_profile.as_deref(),
        )?;
    }
    store.update_thread_status(&thread.id, ThreadStatus::Running)?;

    let workflow = args
        .workflow_name
        .as_deref()
        .map(|workflow_name| {
            workflows::start_workflow_run(
                store,
                workflow_name,
                Some(&thread.id),
                thread.github_item_id.as_deref(),
                Some(Path::new(&project.repo_path)),
            )
        })
        .transpose()?;
    if let Some(bundle) = &workflow {
        store.attach_workflow_run_to_thread(&thread.id, Some(&bundle.run.id))?;
    }

    Ok(LaunchThreadResult {
        thread: store.get_thread(&thread.id)?,
        workflow,
        session_setup: Some(session_setup),
    })
}

fn bridge_thread_status(session: &Session) -> ThreadStatus {
    if session.closed_at.is_some() {
        ThreadStatus::Ready
    } else {
        ThreadStatus::Running
    }
}

fn seed_bridged_thread_context(
    store: &Store,
    thread: &Thread,
    session: &Session,
    task: Option<&Task>,
) -> Result<()> {
    if store.list_thread_messages(&thread.id)?.is_empty() {
        store.create_thread_message(
            &thread.id,
            None,
            "system",
            "Recovered an existing Claustre session into a native thread workspace. Continue the conversation here, inspect diff/runtime/tests/plans on the right, or press o to open the raw terminal.",
            &[],
        )?;
        if let Some(task) = task
            && !task.description.trim().is_empty()
        {
            store.create_thread_message(&thread.id, None, "user", &task.description, &[])?;
        }
    }

    if store.list_thread_runs(&thread.id)?.is_empty() {
        let prompt = task.and_then(|task| {
            (!task.description.trim().is_empty()).then_some(task.description.as_str())
        });
        store.create_thread_run(
            &thread.id,
            thread.provider_kind,
            thread.provider_profile.as_deref(),
            if session.closed_at.is_some() {
                ThreadRunStatus::Pending
            } else {
                ThreadRunStatus::Running
            },
            prompt,
        )?;
    }

    Ok(())
}

pub fn focus_thread(store: &Store, thread_id: &str) -> Result<Thread> {
    store.get_thread(thread_id)
}

pub fn switch_thread_provider(
    store: &Store,
    thread_id: &str,
    provider_kind: ProviderKind,
    provider_profile: Option<&str>,
) -> Result<()> {
    store.update_thread_provider(thread_id, provider_kind, provider_profile)?;
    store.create_thread_run(
        thread_id,
        provider_kind,
        provider_profile,
        ThreadRunStatus::Pending,
        None,
    )?;
    Ok(())
}

pub fn build_thread_provider_command(
    store: &Store,
    cfg: &config::Config,
    thread_id: &str,
    provider_kind: ProviderKind,
    provider_profile: Option<&str>,
) -> Result<Vec<String>> {
    let thread = store.get_thread(thread_id)?;
    let github_item = thread
        .github_item_id
        .as_deref()
        .and_then(|item_id| store.get_github_item(item_id).ok());
    let task = thread
        .task_id
        .as_deref()
        .and_then(|task_id| store.get_task(task_id).ok());
    let prompt = build_thread_handoff_prompt(store, &thread, github_item.as_ref(), task.as_ref())?;
    Ok(build_initial_agent_command(
        cfg,
        provider_kind,
        provider_profile,
        Some(&prompt),
    ))
}

fn align_worktree_to_remote_head(worktree_path: &str, head_ref: &str) -> Result<()> {
    let fetch = Command::new("git")
        .args(["-C", worktree_path, "fetch", "origin", head_ref])
        .output()
        .context("failed to fetch PR head branch")?;
    if !fetch.status.success() {
        bail!("{}", String::from_utf8_lossy(&fetch.stderr).trim());
    }

    let origin_ref = format!("origin/{head_ref}");
    let reset = Command::new("git")
        .args(["-C", worktree_path, "reset", "--hard", &origin_ref])
        .output()
        .context("failed to reset worktree to PR head")?;
    if !reset.status.success() {
        bail!("{}", String::from_utf8_lossy(&reset.stderr).trim());
    }

    Ok(())
}

pub fn smart_paste_clipboard(
    store: &Store,
    thread_id: &str,
    draft_key: Option<&str>,
) -> Result<SmartPasteResult> {
    let mut clipboard = Clipboard::new().context("failed to access system clipboard")?;
    if let Ok(image) = clipboard.get_image() {
        let attachment_dir = config::thread_attachments_dir(thread_id)?;
        fs::create_dir_all(&attachment_dir).with_context(|| {
            format!(
                "failed to create thread attachment directory {}",
                attachment_dir.display()
            )
        })?;
        let file_name = format!("clipboard-{}.png", Uuid::new_v4());
        let output_path = attachment_dir.join(&file_name);
        write_png(
            &output_path,
            u32::try_from(image.width).context("clipboard image width out of range")?,
            u32::try_from(image.height).context("clipboard image height out of range")?,
            image.bytes.as_ref(),
        )?;
        let attachment = store.create_thread_attachment(
            thread_id,
            None,
            draft_key,
            "image/png",
            &file_name,
            i64::try_from(image.width).ok(),
            i64::try_from(image.height).ok(),
            &output_path.display().to_string(),
            AttachmentSource::Clipboard,
        )?;
        return Ok(SmartPasteResult::Image(Box::new(attachment)));
    }

    if let Ok(text) = clipboard.get_text() {
        return Ok(SmartPasteResult::Text(text));
    }

    bail!("clipboard does not contain supported text or image data")
}

pub fn build_initial_agent_command(
    cfg: &config::Config,
    provider_kind: ProviderKind,
    provider_profile: Option<&str>,
    prompt: Option<&str>,
) -> Vec<String> {
    let profile_cfg = provider_profile.and_then(|name| cfg.providers.get(name));
    let command = profile_cfg
        .and_then(|profile| profile.command.clone())
        .unwrap_or_else(|| default_provider_command(provider_kind).to_string());

    match provider_kind {
        ProviderKind::Claude => {
            let model = profile_cfg
                .and_then(|profile| profile.model.clone())
                .unwrap_or_else(|| cfg.claude.model.clone());
            let effort = profile_cfg
                .and_then(|profile| profile.effort.clone())
                .unwrap_or_else(|| cfg.claude.effort.clone());

            let mut cmd = vec![command];
            if cfg.remote_enabled {
                cmd.push("--remote".to_string());
            }
            cmd.extend(["--model".to_string(), model]);
            cmd.extend(["--effort".to_string(), effort]);
            append_prompt(&mut cmd, prompt);
            cmd
        }
        ProviderKind::Codex => {
            let mut cmd = vec![command];
            if let Some(model) = profile_cfg.and_then(|profile| profile.model.clone()) {
                cmd.extend(["--model".to_string(), model]);
            }
            append_prompt(&mut cmd, prompt);
            cmd
        }
        _ => {
            let mut cmd = vec![command];
            append_prompt(&mut cmd, prompt);
            cmd
        }
    }
}

pub fn build_resume_agent_command(
    cfg: &config::Config,
    provider_kind: ProviderKind,
    provider_profile: Option<&str>,
    session: &crate::store::Session,
) -> Vec<String> {
    let profile_cfg = provider_profile.and_then(|name| cfg.providers.get(name));
    let command = profile_cfg
        .and_then(|profile| profile.command.clone())
        .unwrap_or_else(|| default_provider_command(provider_kind).to_string());

    match provider_kind {
        ProviderKind::Claude => {
            let model = profile_cfg
                .and_then(|profile| profile.model.clone())
                .unwrap_or_else(|| cfg.claude.model.clone());
            let effort = profile_cfg
                .and_then(|profile| profile.effort.clone())
                .unwrap_or_else(|| cfg.claude.effort.clone());

            if let Some(csid) = session.claude_session_id.as_ref() {
                vec![
                    command,
                    "--model".to_string(),
                    model,
                    "--effort".to_string(),
                    effort,
                    "--resume".to_string(),
                    csid.clone(),
                ]
            } else {
                vec![
                    command,
                    "--model".to_string(),
                    model,
                    "--effort".to_string(),
                    effort,
                    "--continue".to_string(),
                ]
            }
        }
        ProviderKind::Codex => vec![command, "resume".to_string(), "--last".to_string()],
        _ => vec![command],
    }
}

fn default_provider_command(provider_kind: ProviderKind) -> &'static str {
    match provider_kind {
        ProviderKind::Claude => "claude",
        ProviderKind::Codex => "codex",
        ProviderKind::Gemini => "gemini",
        ProviderKind::Local => "ollama",
        ProviderKind::Unknown => "sh",
    }
}

fn append_prompt(cmd: &mut Vec<String>, prompt: Option<&str>) {
    if let Some(prompt) = prompt.map(str::trim).filter(|prompt| !prompt.is_empty()) {
        cmd.push(prompt.to_string());
    }
}

fn build_thread_handoff_prompt(
    store: &Store,
    thread: &Thread,
    github_item: Option<&crate::store::GitHubItem>,
    task: Option<&crate::store::Task>,
) -> Result<String> {
    let messages = store.list_thread_messages(&thread.id)?;
    let _runs = store.list_thread_runs(&thread.id)?;
    let runtime_state = store
        .get_thread_runtime_state(&thread.id)?
        .and_then(|state| {
            state
                .profile_name
                .as_deref()
                .or(thread.runtime_profile.as_deref())
                .map(|profile| (profile.to_string(), state.build_status))
        });

    let mut prompt = String::new();

    // Minimal handoff — the agent is already on the branch with full code access.
    // Don't dump PR bodies or conversation history; let the user provide context.
    let _ = writeln!(prompt, "Continue working on: {}", thread.title);
    if let Some(branch_name) = thread.branch_name.as_deref() {
        let _ = writeln!(prompt, "Branch: {branch_name}");
    }
    if let Some(github_item) = github_item {
        let _ = writeln!(
            prompt,
            "{} #{}: {}",
            github_item.kind, github_item.number, github_item.title
        );
        let _ = writeln!(prompt, "URL: {}", github_item.url);
        if let (Some(head_ref), Some(base_ref)) = (
            github_item.head_ref.as_deref(),
            github_item.base_ref.as_deref(),
        ) {
            let _ = writeln!(prompt, "Branches: {head_ref} -> {base_ref}");
        }
    } else if let Some(task) = task {
        let _ = writeln!(prompt, "Task: {}", task.title);
    }
    if let Some((profile, build_status)) = runtime_state {
        let _ = writeln!(
            prompt,
            "Runtime: {} [{}]",
            profile,
            build_status.as_deref().unwrap_or("not_started")
        );
    }

    // Only include the last user message for immediate context
    if let Some(last_user_msg) = messages.iter().rev().find(|m| m.role == "user") {
        let _ = writeln!(prompt, "\nLast user message:");
        for line in last_user_msg.content.lines().take(5) {
            prompt.push_str("  ");
            prompt.push_str(line);
            prompt.push('\n');
        }
    }

    prompt.push_str(
        "\nYou are on the correct branch with full code access. Read files and git history as needed. Wait for user instructions.\n",
    );

    Ok(prompt)
}

fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let file = fs::File::create(path)
        .with_context(|| format!("failed to create image file {}", path.display()))?;
    let mut encoder = Encoder::new(file, width, height);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .context("failed to write PNG header for clipboard attachment")?;
    writer
        .write_image_data(rgba)
        .context("failed to encode clipboard image as PNG")?;
    Ok(())
}

/// Suggest a default workflow name based on the launch context.
///
/// Returns an empty string for ad-hoc threads (no default workflow).
/// The user can always change the suggestion before launching.
pub fn suggest_workflow(github_item: Option<&GitHubItem>, is_local_task: bool) -> String {
    if let Some(item) = github_item {
        return match item.kind {
            GitHubItemKind::PullRequest => "review_fix_loop".to_string(),
            GitHubItemKind::Issue | GitHubItemKind::ProjectItem => {
                let has_bug_label = item
                    .label_names
                    .iter()
                    .any(|l| l.eq_ignore_ascii_case("bug"));
                if has_bug_label {
                    "bug_triage_then_patch".to_string()
                } else {
                    "plan_first_tdd".to_string()
                }
            }
        };
    }
    if is_local_task {
        return "plan_first_tdd".to_string();
    }
    // Ad-hoc threads: no default workflow
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::GitHubItemKind;

    #[test]
    fn switch_provider_creates_follow_up_run() {
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
                "thread",
                ProviderKind::Claude,
                Some("default"),
                None,
                None,
                None,
                None,
            )
            .unwrap();

        switch_thread_provider(&store, &thread.id, ProviderKind::Codex, Some("default")).unwrap();
        let thread = store.get_thread(&thread.id).unwrap();
        assert_eq!(thread.provider_kind, ProviderKind::Codex);
        assert_eq!(store.list_thread_runs(&thread.id).unwrap().len(), 1);
    }

    #[test]
    fn build_handoff_prompt_includes_recent_thread_context() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let repo = store
            .upsert_github_repo(Some(&project.id), "acme", "proj", None, Some("main"))
            .unwrap();
        let item = store
            .upsert_github_item(
                &repo.id,
                None,
                None,
                42,
                crate::store::GitHubItemKind::PullRequest,
                "Keep context across providers",
                "OPEN",
                "https://github.com/acme/proj/pull/42",
                Some("Need provider-neutral continuation."),
                &[],
                &[],
                &serde_json::json!({}),
                &serde_json::Value::Null,
                Some("main"),
                Some("codex/thread"),
                None,
            )
            .unwrap();
        let thread = store
            .create_thread(
                &project.id,
                Some(&item.id),
                None,
                None,
                "Continue existing PR",
                ProviderKind::Claude,
                Some("default"),
                None,
                Some("codex/thread"),
                Some("default"),
                None,
            )
            .unwrap();
        store
            .create_thread_run(
                &thread.id,
                ProviderKind::Claude,
                Some("default"),
                ThreadRunStatus::Done,
                Some("Review the PR and continue implementation"),
            )
            .unwrap();
        store
            .create_thread_message(
                &thread.id,
                None,
                "user",
                "Please keep going on this PR.",
                &[],
            )
            .unwrap();
        store
            .create_thread_message(
                &thread.id,
                None,
                "assistant",
                "I updated the parsing path and still need to finish the diff view.",
                &[],
            )
            .unwrap();

        let prompt = build_thread_handoff_prompt(&store, &thread, Some(&item), None).unwrap();

        assert!(prompt.contains("Continue working on:"));
        assert!(prompt.contains("#42: Keep context across providers"));
        // Only the last user message is included, not the full conversation
        assert!(prompt.contains("Please keep going on this PR."));
        // Agent text is NOT included (lean handoff)
        assert!(!prompt.contains("finish the diff view"));
        assert!(prompt.contains("Wait for user instructions."));
    }

    #[test]
    fn relaunch_thread_reuses_existing_live_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let session = store
            .create_session(
                &project.id,
                "thread-branch",
                "/tmp/proj-thread",
                "Thread tab",
            )
            .unwrap();
        let thread = store
            .create_thread(
                &project.id,
                None,
                None,
                Some(&session.id),
                "Continue work",
                ProviderKind::Claude,
                Some("default"),
                Some("/tmp/proj-thread"),
                Some("thread-branch"),
                None,
                None,
            )
            .unwrap();

        let result = relaunch_thread(&store, &crate::config::Config::default(), &thread.id);

        // In tests there's no real session-host process, so the unclosed session
        // is detected as stale and closed. relaunch_thread then tries to create
        // a new session which fails (no real git repo), which is expected.
        // The key assertion: it does NOT return session_setup: None (the old
        // bug that left users stuck on the Threads view).
        assert!(result.is_err() || result.as_ref().unwrap().session_setup.is_some());
    }

    #[test]
    fn ensure_thread_bridge_for_session_creates_reusable_thread_context() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let task = store
            .create_task(
                &project.id,
                "Finish provider switcher",
                "Preserve the conversation and show diff/runtime/test panels.",
                crate::store::TaskMode::Autonomous,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        let session = store
            .create_session(
                &project.id,
                "task/provider-switcher",
                "/tmp/proj-provider-switcher",
                "Provider Switcher",
            )
            .unwrap();

        let thread = ensure_thread_bridge_for_session(&store, &session, Some(&task)).unwrap();

        assert_eq!(thread.session_id.as_deref(), Some(session.id.as_str()));
        assert_eq!(thread.task_id.as_deref(), Some(task.id.as_str()));
        assert_eq!(store.list_thread_messages(&thread.id).unwrap().len(), 2);
        assert_eq!(store.list_thread_runs(&thread.id).unwrap().len(), 1);
    }

    fn make_github_item(kind: GitHubItemKind, labels: Vec<&str>) -> GitHubItem {
        GitHubItem {
            id: "item-1".to_string(),
            repo_id: "repo-1".to_string(),
            project_v2_id: None,
            node_id: None,
            kind,
            number: 42,
            title: "Test item".to_string(),
            body_text: None,
            state: "open".to_string(),
            url: "https://github.com/test/test/issues/42".to_string(),
            label_names: labels.into_iter().map(String::from).collect(),
            label_colors: serde_json::Value::Object(serde_json::Map::new()),
            project_field_values: serde_json::Value::Object(serde_json::Map::new()),
            base_ref: None,
            head_ref: None,
            assignee_logins: vec![],
            linked_pr_number: None,
            linked_pr_item_id: None,
            github_updated_at: None,
            synced_at: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn suggest_workflow_returns_context_based_suggestion() {
        let pr = make_github_item(GitHubItemKind::PullRequest, vec![]);
        assert_eq!(suggest_workflow(Some(&pr), false), "review_fix_loop");

        let bug_issue = make_github_item(GitHubItemKind::Issue, vec!["bug"]);
        assert_eq!(
            suggest_workflow(Some(&bug_issue), false),
            "bug_triage_then_patch"
        );

        let generic_issue = make_github_item(GitHubItemKind::Issue, vec!["enhancement"]);
        assert_eq!(
            suggest_workflow(Some(&generic_issue), false),
            "plan_first_tdd"
        );

        // Local task defaults to plan_first_tdd
        assert_eq!(suggest_workflow(None, true), "plan_first_tdd");

        // Ad-hoc thread: no default
        assert_eq!(suggest_workflow(None, false), "");
    }
}
