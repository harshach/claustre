//! Workflow definition loading and chained stage orchestration.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::store::{
    ProviderKind, Store, WorkflowDef, WorkflowRun, WorkflowRunStatus, WorkflowStageRun,
    WorkflowStageStatus,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub stages: Vec<WorkflowStageDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStageDefinition {
    pub name: String,
    #[serde(default)]
    pub prompt_template: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub provider_profile: Option<String>,
    #[serde(default)]
    pub runtime_profile: Option<String>,
    #[serde(default)]
    pub gate: Option<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct WorkflowRunBundle {
    pub definition: WorkflowDefinition,
    pub stored_definition: WorkflowDef,
    pub run: WorkflowRun,
    pub stages: Vec<WorkflowStageRun>,
}

// ── Markdown format ─────────────────────────────────────────────────
//
// Workflows are stored as `.md` files with YAML frontmatter:
//
//   ---
//   name: plan_first_tdd
//   description: Plan first, then write tests, implement, verify.
//   ---
//
//   ## plan
//   - provider: claude
//
//   Analyze the codebase and produce a detailed implementation plan.
//   IMPORTANT: Do NOT start implementing yet.
//
//   ## approve_plan
//   - gate: manual_approval
//
//   ## write_failing_tests
//   - provider: claude
//   - runtime: default
//
//   Write failing tests that cover the approved plan...

/// Parse a workflow definition from markdown with YAML frontmatter.
pub fn parse_workflow_markdown(content: &str) -> Result<WorkflowDefinition> {
    // Split frontmatter from body
    let (frontmatter, body) = if content.starts_with("---") {
        let rest = &content[3..];
        if let Some(end) = rest.find("\n---") {
            (rest[..end].trim(), rest[end + 4..].trim_start())
        } else {
            ("", content)
        }
    } else {
        ("", content)
    };

    // Parse frontmatter for name and description
    let mut name = String::new();
    let mut description: Option<String> = None;
    for line in frontmatter.lines() {
        if let Some(val) = line.strip_prefix("name:") {
            name = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("description:") {
            description = Some(val.trim().to_string());
        }
    }
    if name.is_empty() {
        anyhow::bail!("workflow markdown missing 'name' in frontmatter");
    }

    // Split body into stage sections by ## headers
    let mut stages = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in body.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            // Flush previous stage
            if let Some(stage_name) = current_name.take() {
                stages.push(parse_stage_section(&stage_name, &current_lines));
                current_lines.clear();
            }
            current_name = Some(header.trim().to_string());
        } else if current_name.is_some() {
            current_lines.push(line);
        }
    }
    // Flush last stage
    if let Some(stage_name) = current_name.take() {
        stages.push(parse_stage_section(&stage_name, &current_lines));
    }

    Ok(WorkflowDefinition {
        name,
        description,
        stages,
    })
}

fn parse_stage_section(name: &str, lines: &[&str]) -> WorkflowStageDefinition {
    let mut provider: Option<String> = None;
    let mut provider_profile: Option<String> = None;
    let mut runtime_profile: Option<String> = None;
    let mut gate: Option<String> = None;
    let mut outputs: Vec<String> = Vec::new();
    let mut prompt_lines: Vec<&str> = Vec::new();
    let mut in_metadata = true;

    for line in lines {
        if in_metadata {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(val) = trimmed.strip_prefix("- provider:") {
                provider = Some(val.trim().to_string());
                continue;
            }
            if let Some(val) = trimmed.strip_prefix("- provider_profile:") {
                provider_profile = Some(val.trim().to_string());
                continue;
            }
            if let Some(val) = trimmed.strip_prefix("- runtime:") {
                runtime_profile = Some(val.trim().to_string());
                continue;
            }
            if let Some(val) = trimmed.strip_prefix("- gate:") {
                let g = val.trim().to_string();
                gate = if g == "null" || g.is_empty() {
                    None
                } else {
                    Some(g)
                };
                continue;
            }
            if let Some(val) = trimmed.strip_prefix("- output:") {
                outputs.push(val.trim().to_string());
                continue;
            }
            // Not a metadata line — switch to prompt content
            in_metadata = false;
            prompt_lines.push(line);
        } else {
            prompt_lines.push(line);
        }
    }

    // Trim leading/trailing empty lines from prompt
    while prompt_lines.first().is_some_and(|l| l.trim().is_empty()) {
        prompt_lines.remove(0);
    }
    while prompt_lines.last().is_some_and(|l| l.trim().is_empty()) {
        prompt_lines.pop();
    }
    let prompt = prompt_lines.join("\n");

    WorkflowStageDefinition {
        name: name.to_string(),
        prompt_template: if prompt.is_empty() {
            None
        } else {
            Some(prompt)
        },
        provider,
        provider_profile,
        runtime_profile,
        gate,
        outputs,
    }
}

/// Serialize a workflow definition to markdown format.
pub fn workflow_to_markdown(definition: &WorkflowDefinition) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("name: {}\n", definition.name));
    if let Some(ref desc) = definition.description {
        out.push_str(&format!("description: {desc}\n"));
    }
    out.push_str("---\n\n");

    for stage in &definition.stages {
        out.push_str(&format!("## {}\n", stage.name));
        if let Some(ref p) = stage.provider {
            out.push_str(&format!("- provider: {p}\n"));
        }
        if let Some(ref pp) = stage.provider_profile {
            if pp != "default" {
                out.push_str(&format!("- provider_profile: {pp}\n"));
            }
        }
        if let Some(ref rp) = stage.runtime_profile {
            out.push_str(&format!("- runtime: {rp}\n"));
        }
        if let Some(ref g) = stage.gate {
            out.push_str(&format!("- gate: {g}\n"));
        }
        for o in &stage.outputs {
            out.push_str(&format!("- output: {o}\n"));
        }
        out.push('\n');
        if let Some(ref prompt) = stage.prompt_template {
            out.push_str(prompt);
            out.push_str("\n\n");
        }
    }
    out
}

pub fn builtin_workflows() -> Vec<WorkflowDefinition> {
    vec![
        WorkflowDefinition {
            name: "plan_first_tdd".to_string(),
            description: Some("Plan first, then write failing tests, implement, verify, and prepare review.".to_string()),
            stages: vec![
                WorkflowStageDefinition {
                    name: "plan".to_string(),
                    prompt_template: Some(
                        "Analyze the codebase and produce a detailed implementation plan. \
                         Read relevant files, understand the architecture, and identify the \
                         specific files and functions that need to change. \
                         Output the plan as a numbered list of concrete steps. \
                         IMPORTANT: Do NOT start implementing or writing code yet — \
                         only produce the plan. The user will review and approve it \
                         before you proceed."
                            .to_string(),
                    ),
                    provider: Some("claude".to_string()),
                    provider_profile: Some("default".to_string()),
                    runtime_profile: None,
                    gate: None,
                    outputs: vec!["plan.md".to_string()],
                },
                WorkflowStageDefinition {
                    name: "approve_plan".to_string(),
                    prompt_template: None,
                    provider: None,
                    provider_profile: None,
                    runtime_profile: None,
                    gate: Some("manual_approval".to_string()),
                    outputs: vec![],
                },
                WorkflowStageDefinition {
                    name: "write_failing_tests".to_string(),
                    prompt_template: Some(
                        "Write failing tests that cover the approved plan. \
                         The tests should fail now (before implementation) and \
                         pass after the implementation is complete. \
                         Do NOT implement the actual changes yet — only write tests."
                            .to_string(),
                    ),
                    provider: Some("claude".to_string()),
                    provider_profile: Some("default".to_string()),
                    runtime_profile: Some("default".to_string()),
                    gate: None,
                    outputs: vec!["test_spec.md".to_string()],
                },
                WorkflowStageDefinition {
                    name: "implement".to_string(),
                    prompt_template: Some(
                        "Implement the approved plan and satisfy the failing tests. \
                         Make the minimum changes needed to make all tests pass."
                            .to_string(),
                    ),
                    provider: Some("claude".to_string()),
                    provider_profile: Some("default".to_string()),
                    runtime_profile: Some("default".to_string()),
                    gate: None,
                    outputs: vec!["implementation.md".to_string()],
                },
                WorkflowStageDefinition {
                    name: "run_checks".to_string(),
                    prompt_template: Some(
                        "Run the project's test suite and linting checks. \
                         Fix any failures. Verify everything passes."
                            .to_string(),
                    ),
                    provider: Some("codex".to_string()),
                    provider_profile: Some("default".to_string()),
                    runtime_profile: Some("default".to_string()),
                    gate: None,
                    outputs: vec!["verification.json".to_string()],
                },
                WorkflowStageDefinition {
                    name: "review_prepare".to_string(),
                    prompt_template: Some(
                        "Summarize the implementation, test evidence, and review risks. \
                         List all files changed, tests added, and any areas that need \
                         careful review."
                            .to_string(),
                    ),
                    provider: Some("claude".to_string()),
                    provider_profile: Some("default".to_string()),
                    runtime_profile: None,
                    gate: None,
                    outputs: vec!["review_summary.md".to_string()],
                },
            ],
        },
        WorkflowDefinition {
            name: "review_fix_loop".to_string(),
            description: Some("Triage review comments, implement accepted fixes, verify, and prepare the next review response.".to_string()),
            stages: vec![
                stage(
                    "triage_review",
                    Some("Read all PR review comments and categorize each as: accept, reject (with reason), or needs-discussion. Output a numbered list with your recommendation for each comment."),
                    "claude",
                ),
                stage(
                    "apply_fixes",
                    Some("Implement the accepted review feedback. For each accepted comment, make the code change and reference the comment number."),
                    "claude",
                ),
                stage(
                    "run_checks",
                    Some("Run the project's test suite and linting checks. Fix any failures. Verify everything passes."),
                    "codex",
                ),
                stage(
                    "prepare_response",
                    Some("Summarize which review comments were addressed and which were rejected with reasons. Prepare a PR comment response."),
                    "claude",
                ),
            ],
        },
        WorkflowDefinition {
            name: "bug_triage_then_patch".to_string(),
            description: Some("Investigate a bug, form a patch plan, implement it, and verify the fix.".to_string()),
            stages: vec![
                stage(
                    "triage",
                    Some("Reproduce and isolate the bug. Read the issue, find the relevant code, identify the root cause. Output your findings and the specific lines/functions responsible."),
                    "claude",
                ),
                stage(
                    "plan_patch",
                    Some("Based on the triage findings, produce a detailed patch plan. List the specific files and functions to change, and describe each change. IMPORTANT: Do NOT start implementing — only produce the plan."),
                    "claude",
                ),
                stage(
                    "implement",
                    Some("Implement the patch plan. Make the minimum changes needed to fix the bug."),
                    "claude",
                ),
                stage(
                    "verify",
                    Some("Run the project's test suite and verify the bug fix. Add a regression test if one doesn't exist."),
                    "codex",
                ),
            ],
        },
        WorkflowDefinition {
            name: "research_then_plan".to_string(),
            description: Some("Research the space first and only then turn it into an actionable plan.".to_string()),
            stages: vec![
                stage(
                    "research",
                    Some("Research the codebase and understand the relevant architecture, patterns, and conventions. Read key files, trace data flows, and identify integration points. Output your findings as a structured summary."),
                    "claude",
                ),
                stage(
                    "plan",
                    Some("Based on the research findings, produce a detailed implementation plan with numbered steps. Identify files to change, functions to add/modify, and potential risks. IMPORTANT: Do NOT start implementing — only produce the plan."),
                    "claude",
                ),
            ],
        },
    ]
}

/// Built-in workflow markdown files, embedded at compile time from `assets/workflows/`.
/// These are written to `~/.claustre/workflows/` on first run so users can edit them.
const BUILTIN_WORKFLOW_FILES: &[(&str, &str)] = &[
    (
        "plan_first_tdd.md",
        include_str!("../assets/workflows/plan_first_tdd.md"),
    ),
    (
        "design_first.md",
        include_str!("../assets/workflows/design_first.md"),
    ),
    (
        "bug_triage_then_patch.md",
        include_str!("../assets/workflows/bug_triage_then_patch.md"),
    ),
    (
        "review_fix_loop.md",
        include_str!("../assets/workflows/review_fix_loop.md"),
    ),
];

/// Seed built-in workflows as markdown files in `~/.claustre/workflows/` if
/// they don't already exist on disk (checks both `.md` and legacy `.yaml`).
/// This lets users edit the default workflows without recompiling, and ensures
/// new built-in workflows are added on upgrade.
pub fn seed_builtin_workflows() -> Result<()> {
    let dir = config::workflows_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create workflows directory {}", dir.display()))?;
    for &(filename, content) in BUILTIN_WORKFLOW_FILES {
        let md_path = dir.join(filename);
        let stem = filename.strip_suffix(".md").unwrap_or(filename);
        let yaml_path = dir.join(format!("{stem}.yaml"));
        // Don't overwrite if either format exists (user may have edited)
        if !md_path.exists() && !yaml_path.exists() {
            fs::write(&md_path, content)
                .with_context(|| format!("failed to seed workflow file {}", md_path.display()))?;
        }
    }
    Ok(())
}

pub fn load_workflow_definitions(repo_root: Option<&Path>) -> Result<Vec<WorkflowDefinition>> {
    // Load custom/seeded files from disk first — these take precedence over built-ins.
    let mut defs: Vec<WorkflowDefinition> = Vec::new();
    for path in workflow_definition_paths(repo_root)? {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read workflow file {}", path.display()))?;
        let definition = if path.extension().and_then(|e| e.to_str()) == Some("md") {
            parse_workflow_markdown(&content)
                .with_context(|| format!("failed to parse workflow markdown {}", path.display()))?
        } else {
            serde_yaml::from_str(&content)
                .with_context(|| format!("failed to parse workflow YAML {}", path.display()))?
        };
        defs.push(definition);
    }
    // Add built-ins that aren't overridden by on-disk files.
    for builtin in builtin_workflows() {
        if !defs.iter().any(|d| d.name == builtin.name) {
            defs.push(builtin);
        }
    }
    Ok(defs)
}

pub fn sync_workflow_definitions(
    store: &Store,
    repo_root: Option<&Path>,
) -> Result<Vec<(WorkflowDefinition, WorkflowDef)>> {
    load_workflow_definitions(repo_root)?
        .into_iter()
        .map(|definition| {
            let source_path = workflow_definition_paths(repo_root)?
                .into_iter()
                .find(|path| {
                    path.file_stem().and_then(|stem| stem.to_str())
                        == Some(definition.name.as_str())
                });
            let source_path_string = source_path.as_ref().map(|path| path.display().to_string());
            let scope = if source_path.is_some() {
                if let Some(repo_root) = repo_root {
                    if source_path
                        .as_ref()
                        .is_some_and(|path| path.starts_with(config::repo_workflows_dir(repo_root)))
                    {
                        "repo"
                    } else {
                        "global"
                    }
                } else {
                    "global"
                }
            } else {
                "builtin"
            };
            let yaml = serde_yaml::to_string(&definition)?;
            let stored = store.upsert_workflow_def(
                &definition.name,
                scope,
                source_path_string.as_deref(),
                definition.description.as_deref(),
                &yaml,
                source_path.is_none(),
            )?;
            Ok((definition, stored))
        })
        .collect()
}

pub fn start_workflow_run(
    store: &Store,
    workflow_name: &str,
    thread_id: Option<&str>,
    github_item_id: Option<&str>,
    repo_root: Option<&Path>,
) -> Result<WorkflowRunBundle> {
    let definitions = sync_workflow_definitions(store, repo_root)?;
    let (definition, stored_definition) = definitions
        .into_iter()
        .find(|(definition, _)| definition.name == workflow_name)
        .with_context(|| format!("workflow '{workflow_name}' not found"))?;

    let first_stage = definition
        .stages
        .first()
        .with_context(|| format!("workflow '{workflow_name}' has no stages"))?;
    let initial_status = if first_stage.gate.as_deref() == Some("manual_approval") {
        WorkflowRunStatus::WaitingApproval
    } else {
        WorkflowRunStatus::Running
    };
    let run = store.create_workflow_run(
        &stored_definition.id,
        thread_id,
        github_item_id,
        initial_status,
        Some(&first_stage.name),
    )?;
    let mut stage_runs = Vec::with_capacity(definition.stages.len());
    for (index, stage) in definition.stages.iter().enumerate() {
        let status = if index == 0 {
            if stage.gate.as_deref() == Some("manual_approval") {
                WorkflowStageStatus::WaitingApproval
            } else {
                WorkflowStageStatus::Running
            }
        } else {
            WorkflowStageStatus::Pending
        };
        stage_runs.push(
            store.create_workflow_stage_run(
                &run.id,
                &stage.name,
                status,
                stage
                    .provider
                    .as_deref()
                    .and_then(|provider| provider.parse::<ProviderKind>().ok()),
                stage.provider_profile.as_deref(),
                stage.runtime_profile.as_deref(),
                stage.prompt_template.as_deref(),
                stage.gate.as_deref(),
                None,
            )?,
        );
    }

    Ok(WorkflowRunBundle {
        definition,
        stored_definition,
        run,
        stages: stage_runs,
    })
}

pub fn resume_workflow_run(store: &Store, run_id: &str) -> Result<WorkflowRunBundle> {
    let run = store.get_workflow_run(run_id)?;
    let stored_definition = store.get_workflow_def(&run.workflow_def_id)?;
    let definition: WorkflowDefinition = serde_yaml::from_str(&stored_definition.definition_yaml)
        .with_context(|| {
        format!(
            "failed to parse stored workflow definition '{}'",
            stored_definition.name
        )
    })?;
    let mut stages = store.list_workflow_stage_runs(run_id)?;

    if let Some(current_running) = stages
        .iter()
        .find(|stage| stage.status == WorkflowStageStatus::Running)
    {
        store.update_workflow_stage_run_status(
            &current_running.id,
            WorkflowStageStatus::Completed,
            Some("auto_advanced"),
            None,
        )?;
    }

    stages = store.list_workflow_stage_runs(run_id)?;
    let next_pending = stages
        .iter()
        .find(|stage| stage.status == WorkflowStageStatus::Pending);

    match next_pending {
        Some(next_stage) => {
            let stage_def = definition
                .stages
                .iter()
                .find(|stage| stage.name == next_stage.stage_name)
                .with_context(|| {
                    format!(
                        "workflow definition missing stage '{}'",
                        next_stage.stage_name
                    )
                })?;
            let next_status = if stage_def.gate.as_deref() == Some("manual_approval") {
                WorkflowStageStatus::WaitingApproval
            } else {
                WorkflowStageStatus::Running
            };
            store.update_workflow_stage_run_status(
                &next_stage.id,
                next_status,
                stage_def.gate.as_deref(),
                None,
            )?;
            store.update_workflow_run_status(
                run_id,
                if next_status == WorkflowStageStatus::WaitingApproval {
                    WorkflowRunStatus::WaitingApproval
                } else {
                    WorkflowRunStatus::Running
                },
                Some(&next_stage.stage_name),
            )?;
        }
        None => {
            store.update_workflow_run_status(run_id, WorkflowRunStatus::Completed, None)?;
        }
    }

    Ok(WorkflowRunBundle {
        definition,
        stored_definition,
        run: store.get_workflow_run(run_id)?,
        stages: store.list_workflow_stage_runs(run_id)?,
    })
}

pub fn approve_workflow_stage(
    store: &Store,
    run_id: &str,
    stage_name: &str,
) -> Result<WorkflowRunBundle> {
    let stage = store
        .list_workflow_stage_runs(run_id)?
        .into_iter()
        .find(|stage| stage.stage_name == stage_name)
        .with_context(|| format!("workflow run '{run_id}' has no stage named '{stage_name}'"))?;
    store.update_workflow_stage_run_status(
        &stage.id,
        WorkflowStageStatus::Completed,
        Some("approved"),
        stage.output_summary.as_deref(),
    )?;
    resume_workflow_run(store, run_id)
}

fn workflow_definition_paths(repo_root: Option<&Path>) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_workflow_paths(&config::workflows_dir()?, &mut paths)?;
    if let Some(repo_root) = repo_root {
        collect_workflow_paths(&config::repo_workflows_dir(repo_root), &mut paths)?;
    }
    Ok(paths)
}

fn collect_workflow_paths(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)
        .with_context(|| format!("failed to read workflow directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| matches!(extension, "md" | "yaml" | "yml"))
        {
            paths.push(path);
        }
    }
    Ok(())
}

/// Save a custom workflow definition to the global workflows directory as markdown.
pub fn save_workflow_definition(definition: &WorkflowDefinition) -> Result<()> {
    let dir = config::workflows_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create workflow directory {}", dir.display()))?;
    let path = dir.join(format!("{}.md", definition.name));
    let markdown = workflow_to_markdown(definition);
    fs::write(&path, markdown)
        .with_context(|| format!("failed to write workflow file {}", path.display()))?;
    Ok(())
}

/// Delete a custom workflow definition from the global workflows directory.
///
/// Returns `Ok(true)` if the file existed and was removed, `Ok(false)` if not found.
pub fn delete_workflow_definition(name: &str) -> Result<bool> {
    let dir = config::workflows_dir()?;
    // Try .md first (new format), then .yaml/.yml (legacy)
    for ext in ["md", "yaml", "yml"] {
        let path = dir.join(format!("{name}.{ext}"));
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove workflow file {}", path.display()))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Check if a workflow name is a built-in (non-deletable) workflow.
pub fn is_builtin_workflow(name: &str) -> bool {
    builtin_workflows().iter().any(|w| w.name == name)
}

fn stage(name: &str, prompt_template: Option<&str>, provider: &str) -> WorkflowStageDefinition {
    WorkflowStageDefinition {
        name: name.to_string(),
        prompt_template: prompt_template.map(str::to_string),
        provider: Some(provider.to_string()),
        provider_profile: Some("default".to_string()),
        runtime_profile: Some("default".to_string()),
        gate: None,
        outputs: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn builtin_workflow_contains_plan_first_tdd() {
        let definitions = builtin_workflows();
        assert!(
            definitions
                .iter()
                .any(|definition| definition.name == "plan_first_tdd")
        );
    }

    #[test]
    fn start_and_resume_workflow_run_progresses_stages() {
        let store = Store::open_in_memory().unwrap();
        let bundle = start_workflow_run(&store, "research_then_plan", None, None, None).unwrap();
        assert_eq!(bundle.run.status, WorkflowRunStatus::Running);
        let resumed = resume_workflow_run(&store, &bundle.run.id).unwrap();
        assert_eq!(resumed.stages[0].status, WorkflowStageStatus::Completed);
    }
}
