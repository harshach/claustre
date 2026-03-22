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

pub fn builtin_workflows() -> Vec<WorkflowDefinition> {
    vec![
        WorkflowDefinition {
            name: "plan_first_tdd".to_string(),
            description: Some("Plan first, then write failing tests, implement, verify, and prepare review.".to_string()),
            stages: vec![
                WorkflowStageDefinition {
                    name: "plan".to_string(),
                    prompt_template: Some("/plan".to_string()),
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
                    prompt_template: Some("/tdd".to_string()),
                    provider: Some("claude".to_string()),
                    provider_profile: Some("default".to_string()),
                    runtime_profile: Some("default".to_string()),
                    gate: None,
                    outputs: vec!["test_spec.md".to_string()],
                },
                WorkflowStageDefinition {
                    name: "implement".to_string(),
                    prompt_template: Some("Implement the approved plan and satisfy the failing tests.".to_string()),
                    provider: Some("claude".to_string()),
                    provider_profile: Some("default".to_string()),
                    runtime_profile: Some("default".to_string()),
                    gate: None,
                    outputs: vec!["implementation.md".to_string()],
                },
                WorkflowStageDefinition {
                    name: "run_checks".to_string(),
                    prompt_template: Some("/verify".to_string()),
                    provider: Some("codex".to_string()),
                    provider_profile: Some("default".to_string()),
                    runtime_profile: Some("default".to_string()),
                    gate: None,
                    outputs: vec!["verification.json".to_string()],
                },
                WorkflowStageDefinition {
                    name: "review_prepare".to_string(),
                    prompt_template: Some("Summarize the implementation, test evidence, and review risks.".to_string()),
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
                stage("triage_review", Some("/review"), "claude"),
                stage("apply_fixes", Some("Implement accepted review feedback."), "claude"),
                stage("run_checks", Some("/verify"), "codex"),
                stage("prepare_response", Some("Summarize addressed and rejected feedback."), "claude"),
            ],
        },
        WorkflowDefinition {
            name: "bug_triage_then_patch".to_string(),
            description: Some("Investigate a bug, form a patch plan, implement it, and verify the fix.".to_string()),
            stages: vec![
                stage("triage", Some("Reproduce and isolate the bug."), "claude"),
                stage("plan_patch", Some("/plan"), "claude"),
                stage("implement", Some("Implement the chosen patch."), "claude"),
                stage("verify", Some("/verify"), "codex"),
            ],
        },
        WorkflowDefinition {
            name: "research_then_plan".to_string(),
            description: Some("Research the space first and only then turn it into an actionable plan.".to_string()),
            stages: vec![
                stage("research", Some("/learn"), "claude"),
                stage("plan", Some("/plan"), "claude"),
            ],
        },
    ]
}

pub fn load_workflow_definitions(repo_root: Option<&Path>) -> Result<Vec<WorkflowDefinition>> {
    let mut defs = builtin_workflows();
    for path in workflow_definition_paths(repo_root)? {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read workflow file {}", path.display()))?;
        let definition: WorkflowDefinition = serde_yaml::from_str(&content)
            .with_context(|| format!("failed to parse workflow file {}", path.display()))?;
        defs.push(definition);
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
            .is_some_and(|extension| matches!(extension, "yaml" | "yml"))
        {
            paths.push(path);
        }
    }
    Ok(())
}

/// Save a custom workflow definition to the global workflows directory.
pub fn save_workflow_definition(definition: &WorkflowDefinition) -> Result<()> {
    let dir = config::workflows_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create workflow directory {}", dir.display()))?;
    let path = dir.join(format!("{}.yaml", definition.name));
    let yaml = serde_yaml::to_string(definition)
        .context("failed to serialize workflow definition to YAML")?;
    fs::write(&path, yaml)
        .with_context(|| format!("failed to write workflow file {}", path.display()))?;
    Ok(())
}

/// Delete a custom workflow definition from the global workflows directory.
///
/// Returns `Ok(true)` if the file existed and was removed, `Ok(false)` if not found.
pub fn delete_workflow_definition(name: &str) -> Result<bool> {
    let dir = config::workflows_dir()?;
    let path = dir.join(format!("{name}.yaml"));
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove workflow file {}", path.display()))?;
        Ok(true)
    } else {
        // Try .yml extension
        let path_yml = dir.join(format!("{name}.yml"));
        if path_yml.exists() {
            fs::remove_file(&path_yml).with_context(|| {
                format!("failed to remove workflow file {}", path_yml.display())
            })?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
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
