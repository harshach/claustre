//! `sandbox.yaml` loading and worktree runtime orchestration.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::store::{RuntimeServiceKind, RuntimeServiceStatus, Store, ThreadStatus};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxConfig {
    pub version: i64,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub build: Vec<BuildStep>,
    #[serde(default)]
    pub services: Vec<ServiceConfig>,
    #[serde(default)]
    pub checks: Vec<CheckConfig>,
    #[serde(default)]
    pub profiles: BTreeMap<String, RuntimeProfile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BuildStep {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_required_on_launch")]
    pub required_on_launch: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckConfig {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub kind: RuntimeServiceKind,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub ready: Option<ReadyProbe>,
    #[serde(default)]
    pub stop: Option<String>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeProfile {
    #[serde(default)]
    pub build: Vec<String>,
    #[serde(default)]
    pub services: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default = "default_true")]
    pub build_on_launch: bool,
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub default_inspector_tab: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadyProbe {
    #[serde(default)]
    pub http: Option<String>,
    #[serde(default)]
    pub tcp: Option<String>,
    #[serde(default)]
    pub exec: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeServiceState {
    pub name: String,
    pub kind: RuntimeServiceKind,
    pub status: RuntimeServiceStatus,
    pub pid: Option<u32>,
    pub log_path: Option<String>,
    pub health: Option<String>,
    pub started_at: String,
}

#[derive(Debug, Clone)]
pub struct LoadedSandbox {
    pub path: PathBuf,
    pub config: SandboxConfig,
}

#[derive(Debug, Clone)]
pub struct ResolvedProfile<'a> {
    pub name: String,
    pub build_steps: Vec<&'a BuildStep>,
    pub services: Vec<&'a ServiceConfig>,
    pub checks: Vec<&'a CheckConfig>,
    pub build_on_launch: bool,
}

fn default_required_on_launch() -> bool {
    true
}

fn default_true() -> bool {
    true
}

pub fn discover_sandbox(
    worktree_root: &Path,
    repo_root: &Path,
    runtime_cfg: &config::RuntimeConfig,
) -> Result<Option<LoadedSandbox>> {
    let candidates = runtime_cfg
        .sandbox_path
        .as_deref()
        .map(PathBuf::from)
        .into_iter()
        .chain([worktree_root.join("sandbox.yaml")])
        .chain([config::repo_sandbox_path(repo_root)])
        .chain([config::base_dir()?.join("sandbox.yaml")]);

    for candidate in candidates {
        if candidate.exists() {
            let content = fs::read_to_string(&candidate)
                .with_context(|| format!("failed to read sandbox file {}", candidate.display()))?;
            let sandbox: SandboxConfig = serde_yaml::from_str(&content)
                .with_context(|| format!("failed to parse sandbox file {}", candidate.display()))?;
            anyhow::ensure!(
                sandbox.version == 1,
                "unsupported sandbox version {} in {}",
                sandbox.version,
                candidate.display()
            );
            return Ok(Some(LoadedSandbox {
                path: candidate,
                config: sandbox,
            }));
        }
    }

    Ok(None)
}

pub fn resolve_profile<'a>(
    sandbox: &'a SandboxConfig,
    profile_name: Option<&str>,
) -> Result<ResolvedProfile<'a>> {
    let requested_name = profile_name
        .or_else(|| (!sandbox.profiles.is_empty()).then_some("default"))
        .map_or_else(|| "default".to_string(), str::to_string);

    if sandbox.profiles.is_empty() {
        return Ok(ResolvedProfile {
            name: requested_name,
            build_steps: sandbox
                .build
                .iter()
                .filter(|step| step.required_on_launch)
                .collect(),
            services: sandbox
                .services
                .iter()
                .filter(|service| service.auto_start)
                .collect(),
            checks: sandbox.checks.iter().collect(),
            build_on_launch: true,
        });
    }

    let profile = sandbox
        .profiles
        .get(&requested_name)
        .with_context(|| format!("runtime profile '{requested_name}' not found"))?;

    let build_map: HashMap<&str, &BuildStep> = sandbox
        .build
        .iter()
        .map(|step| (step.name.as_str(), step))
        .collect();
    let services_map: HashMap<&str, &ServiceConfig> = sandbox
        .services
        .iter()
        .map(|service| (service.name.as_str(), service))
        .collect();
    let checks_map: HashMap<&str, &CheckConfig> = sandbox
        .checks
        .iter()
        .map(|check| (check.name.as_str(), check))
        .collect();

    let build_steps = profile
        .build
        .iter()
        .map(|name| {
            build_map.get(name.as_str()).copied().with_context(|| {
                format!("profile '{requested_name}' references unknown build step '{name}'")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let services = profile
        .services
        .iter()
        .map(|name| {
            services_map.get(name.as_str()).copied().with_context(|| {
                format!("profile '{requested_name}' references unknown service '{name}'")
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let checks = profile
        .checks
        .iter()
        .map(|name| {
            checks_map.get(name.as_str()).copied().with_context(|| {
                format!("profile '{requested_name}' references unknown check '{name}'")
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(ResolvedProfile {
        name: requested_name,
        build_steps,
        services,
        checks,
        build_on_launch: profile.build_on_launch,
    })
}

pub fn build_profile(
    worktree_root: &Path,
    sandbox: &SandboxConfig,
    profile: &ResolvedProfile<'_>,
) -> Result<()> {
    for step in &profile.build_steps {
        run_shell_command(
            step.run.as_str(),
            resolve_cwd(worktree_root, step.cwd.as_deref()),
            &merge_env(&sandbox.env, &step.env),
            None,
        )
        .with_context(|| format!("build step '{}' failed", step.name))?;
    }
    Ok(())
}

pub fn run_check(worktree_root: &Path, sandbox: &SandboxConfig, check: &CheckConfig) -> Result<()> {
    run_shell_command(
        check.run.as_str(),
        resolve_cwd(worktree_root, check.cwd.as_deref()),
        &merge_env(&sandbox.env, &check.env),
        None,
    )
    .with_context(|| format!("check '{}' failed", check.name))
}

pub fn up_profile(
    store: &Store,
    thread_id: &str,
    worktree_root: &Path,
    repo_root: &Path,
    runtime_cfg: &config::RuntimeConfig,
    requested_profile: Option<&str>,
) -> Result<Vec<RuntimeServiceState>> {
    let sandbox = discover_sandbox(worktree_root, repo_root, runtime_cfg)?
        .context("no sandbox.yaml found for thread runtime")?;
    let resolved = resolve_profile(
        &sandbox.config,
        requested_profile.or(runtime_cfg.default_profile.as_deref()),
    )?;

    store.update_thread_status(thread_id, ThreadStatus::RuntimePreparing)?;

    if resolved.build_on_launch {
        build_profile(worktree_root, &sandbox.config, &resolved)?;
    }

    let mut service_states = Vec::with_capacity(resolved.services.len());
    let runtime_dir = config::thread_runtime_dir(thread_id)?;
    fs::create_dir_all(&runtime_dir).with_context(|| {
        format!(
            "failed to create runtime directory {}",
            runtime_dir.display()
        )
    })?;

    for &service in &resolved.services {
        let state = match service.kind {
            RuntimeServiceKind::Command => {
                let cmd = service.run.as_deref().with_context(|| {
                    format!(
                        "service '{}' is missing `run` for kind=command",
                        service.name
                    )
                })?;
                let log_path = runtime_dir.join(format!("{}.log", service.name));
                let log = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .with_context(|| {
                        format!("failed to open runtime log {}", log_path.display())
                    })?;
                let log_err = log
                    .try_clone()
                    .context("failed to clone runtime log handle")?;
                let mut command = Command::new("/bin/bash");
                command.arg("-lc").arg(cmd);
                command.current_dir(resolve_cwd(worktree_root, service.cwd.as_deref()));
                command.stdin(Stdio::null());
                command.stdout(Stdio::from(log));
                command.stderr(Stdio::from(log_err));
                for (key, value) in merge_env(&sandbox.config.env, &service.env) {
                    command.env(key, value);
                }
                let child = command
                    .spawn()
                    .with_context(|| format!("failed to start service '{}'", service.name))?;
                RuntimeServiceState {
                    name: service.name.clone(),
                    kind: service.kind,
                    status: RuntimeServiceStatus::Running,
                    pid: Some(child.id()),
                    log_path: Some(log_path.display().to_string()),
                    health: None,
                    started_at: chrono::Utc::now().to_rfc3339(),
                }
            }
            RuntimeServiceKind::Compose => {
                let compose_file = resolve_cwd(worktree_root, service.file.as_deref());
                let service_name = service.service.as_deref().with_context(|| {
                    format!(
                        "service '{}' is missing `service` for kind=compose",
                        service.name
                    )
                })?;
                let mut args = vec!["compose".to_string()];
                if let Some(project_name) = service.project_name.as_deref() {
                    args.push("-p".to_string());
                    args.push(project_name.to_string());
                }
                args.push("-f".to_string());
                args.push(compose_file.display().to_string());
                args.push("up".to_string());
                args.push("-d".to_string());
                args.push(service_name.to_string());
                let output = Command::new("docker")
                    .args(&args)
                    .current_dir(resolve_cwd(worktree_root, service.cwd.as_deref()))
                    .output()
                    .with_context(|| {
                        format!("failed to start compose service '{}'", service.name)
                    })?;
                if !output.status.success() {
                    bail!(
                        "docker compose failed for service '{}': {}",
                        service.name,
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                RuntimeServiceState {
                    name: service.name.clone(),
                    kind: service.kind,
                    status: RuntimeServiceStatus::Running,
                    pid: None,
                    log_path: None,
                    health: None,
                    started_at: chrono::Utc::now().to_rfc3339(),
                }
            }
        };
        service_states.push(state);
    }

    let mut state_with_health = service_states;
    for (service_state, service_cfg) in state_with_health.iter_mut().zip(resolved.services.iter()) {
        if let Some(ready) = &service_cfg.ready {
            wait_for_ready_probe(worktree_root, ready).with_context(|| {
                format!("service '{}' failed readiness checks", service_cfg.name)
            })?;
            service_state.status = RuntimeServiceStatus::Healthy;
            service_state.health = Some("ready".to_string());
        }
    }

    store.upsert_thread_runtime_state(
        thread_id,
        Some(&resolved.name),
        Some("ready"),
        &serde_json::to_string(&state_with_health)?,
        None,
    )?;
    store.update_thread_status(thread_id, ThreadStatus::Ready)?;

    Ok(state_with_health)
}

pub fn down_profile(
    store: &Store,
    thread_id: &str,
    worktree_root: &Path,
    repo_root: &Path,
    runtime_cfg: &config::RuntimeConfig,
) -> Result<Vec<RuntimeServiceState>> {
    let thread = store.get_thread(thread_id)?;
    let sandbox = discover_sandbox(worktree_root, repo_root, runtime_cfg)?
        .context("no sandbox.yaml found for thread runtime")?;
    let resolved = resolve_profile(
        &sandbox.config,
        thread
            .runtime_profile
            .as_deref()
            .or(runtime_cfg.default_profile.as_deref()),
    )?;
    let existing = store
        .get_thread_runtime_state(thread_id)?
        .map(|state| serde_json::from_str::<Vec<RuntimeServiceState>>(&state.services_json))
        .transpose()?
        .unwrap_or_default();

    let state_by_name: HashMap<String, RuntimeServiceState> = existing
        .into_iter()
        .map(|state| (state.name.clone(), state))
        .collect();
    let mut stopped = Vec::with_capacity(resolved.services.len());

    for service in resolved.services.iter().rev() {
        match service.kind {
            RuntimeServiceKind::Command => {
                if let Some(state) = state_by_name.get(&service.name)
                    && let Some(pid) = state.pid
                {
                    stop_command_service(
                        pid,
                        service.stop.as_deref(),
                        resolve_cwd(worktree_root, service.cwd.as_deref()),
                    )?;
                }
            }
            RuntimeServiceKind::Compose => {
                let compose_file = resolve_cwd(worktree_root, service.file.as_deref());
                let service_name = service.service.as_deref().with_context(|| {
                    format!(
                        "service '{}' is missing `service` for kind=compose",
                        service.name
                    )
                })?;
                let mut args = vec!["compose".to_string()];
                if let Some(project_name) = service.project_name.as_deref() {
                    args.push("-p".to_string());
                    args.push(project_name.to_string());
                }
                args.push("-f".to_string());
                args.push(compose_file.display().to_string());
                args.push("stop".to_string());
                args.push(service_name.to_string());
                let output = Command::new("docker")
                    .args(&args)
                    .current_dir(resolve_cwd(worktree_root, service.cwd.as_deref()))
                    .output()
                    .with_context(|| {
                        format!("failed to stop compose service '{}'", service.name)
                    })?;
                if !output.status.success() {
                    bail!(
                        "docker compose stop failed for service '{}': {}",
                        service.name,
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
        stopped.push(RuntimeServiceState {
            name: service.name.clone(),
            kind: service.kind,
            status: RuntimeServiceStatus::Stopped,
            pid: None,
            log_path: state_by_name
                .get(&service.name)
                .and_then(|existing| existing.log_path.clone()),
            health: None,
            started_at: chrono::Utc::now().to_rfc3339(),
        });
    }

    store.upsert_thread_runtime_state(
        thread_id,
        thread.runtime_profile.as_deref(),
        Some("stopped"),
        &serde_json::to_string(&stopped)?,
        None,
    )?;
    Ok(stopped)
}

pub fn restart_profile(
    store: &Store,
    thread_id: &str,
    worktree_root: &Path,
    repo_root: &Path,
    runtime_cfg: &config::RuntimeConfig,
) -> Result<Vec<RuntimeServiceState>> {
    let _ = down_profile(store, thread_id, worktree_root, repo_root, runtime_cfg);
    up_profile(
        store,
        thread_id,
        worktree_root,
        repo_root,
        runtime_cfg,
        None,
    )
}

pub fn health_check_profile(
    store: &Store,
    thread_id: &str,
    worktree_root: &Path,
    repo_root: &Path,
    runtime_cfg: &config::RuntimeConfig,
) -> Result<Vec<RuntimeServiceState>> {
    let thread = store.get_thread(thread_id)?;
    let sandbox = discover_sandbox(worktree_root, repo_root, runtime_cfg)?
        .context("no sandbox.yaml found for thread runtime")?;
    let resolved = resolve_profile(
        &sandbox.config,
        thread
            .runtime_profile
            .as_deref()
            .or(runtime_cfg.default_profile.as_deref()),
    )?;
    let current = store
        .get_thread_runtime_state(thread_id)?
        .map(|state| serde_json::from_str::<Vec<RuntimeServiceState>>(&state.services_json))
        .transpose()?
        .unwrap_or_default();
    let mut current_by_name: HashMap<String, RuntimeServiceState> = current
        .into_iter()
        .map(|state| (state.name.clone(), state))
        .collect();

    for service in resolved.services {
        if let Some(ready) = &service.ready {
            wait_for_ready_probe(worktree_root, ready)
                .with_context(|| format!("service '{}' failed readiness checks", service.name))?;
        }
        let entry = current_by_name
            .entry(service.name.clone())
            .or_insert_with(|| RuntimeServiceState {
                name: service.name.clone(),
                kind: service.kind,
                status: RuntimeServiceStatus::Running,
                pid: None,
                log_path: None,
                health: None,
                started_at: chrono::Utc::now().to_rfc3339(),
            });
        entry.status = RuntimeServiceStatus::Healthy;
        entry.health = Some("ready".to_string());
    }

    let states = current_by_name.into_values().collect::<Vec<_>>();
    store.upsert_thread_runtime_state(
        thread_id,
        thread.runtime_profile.as_deref(),
        Some("healthy"),
        &serde_json::to_string(&states)?,
        None,
    )?;
    Ok(states)
}

pub fn service_log_path(
    store: &Store,
    thread_id: &str,
    service_name: &str,
) -> Result<Option<PathBuf>> {
    let Some(runtime_state) = store.get_thread_runtime_state(thread_id)? else {
        return Ok(None);
    };
    let states: Vec<RuntimeServiceState> = serde_json::from_str(&runtime_state.services_json)?;
    Ok(states
        .into_iter()
        .find(|service| service.name == service_name)
        .and_then(|service| service.log_path.map(PathBuf::from)))
}

fn run_shell_command(
    command: &str,
    cwd: PathBuf,
    env: &BTreeMap<String, String>,
    output_path: Option<&Path>,
) -> Result<()> {
    let mut cmd = Command::new("/bin/bash");
    cmd.arg("-lc").arg(command);
    cmd.current_dir(cwd);
    for (key, value) in env {
        cmd.env(key, value);
    }
    if let Some(output_path) = output_path {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(output_path)
            .with_context(|| format!("failed to open {}", output_path.display()))?;
        let stderr = file
            .try_clone()
            .context("failed to duplicate file handle")?;
        cmd.stdout(Stdio::from(file));
        cmd.stderr(Stdio::from(stderr));
    }
    let status = cmd.status().context("failed to execute shell command")?;
    if !status.success() {
        bail!(
            "command '{}' failed with status {}",
            command,
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

fn resolve_cwd(worktree_root: &Path, maybe_cwd: Option<&str>) -> PathBuf {
    maybe_cwd.map_or_else(
        || worktree_root.to_path_buf(),
        |cwd| worktree_root.join(cwd),
    )
}

fn merge_env(
    base: &BTreeMap<String, String>,
    extra: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = base.clone();
    merged.extend(extra.clone());
    merged
}

fn wait_for_ready_probe(worktree_root: &Path, ready: &ReadyProbe) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let result = if let Some(http_url) = &ready.http {
            probe_http(http_url)
        } else if let Some(tcp_addr) = &ready.tcp {
            probe_tcp(tcp_addr)
        } else if let Some(exec_cmd) = &ready.exec {
            run_shell_command(
                exec_cmd,
                worktree_root.to_path_buf(),
                &BTreeMap::new(),
                None,
            )
        } else {
            Ok(())
        };

        if result.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return result;
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn probe_tcp(address: &str) -> Result<()> {
    TcpStream::connect(address)
        .with_context(|| format!("failed TCP health probe for {address}"))?;
    Ok(())
}

fn probe_http(url: &str) -> Result<()> {
    let stripped = url
        .strip_prefix("http://")
        .with_context(|| format!("only http:// URLs are supported for health probes: {url}"))?;
    let (host_port, path) = stripped.split_once('/').map_or_else(
        || (stripped, "/".to_string()),
        |(host_port, rest)| (host_port, format!("/{rest}")),
    );
    let mut stream = TcpStream::connect(host_port)
        .with_context(|| format!("failed HTTP health probe for {url}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .with_context(|| format!("failed to write HTTP health probe for {url}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .with_context(|| format!("failed to read HTTP health probe response for {url}"))?;
    let status_line = response.lines().next().unwrap_or_default();
    anyhow::ensure!(
        status_line.contains(" 200 ") || status_line.contains(" 204 "),
        "HTTP health probe failed for {url}: {status_line}"
    );
    Ok(())
}

// ── User-facing sandbox.yaml types ──────────────────────────────────────────

/// Top-level user-facing `sandbox.yaml` format.
///
/// This is the simplified format users place in their repository root.
/// It describes the project's sandbox environment: languages, Docker services,
/// test suites, and instructions for Claude.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UserSandboxConfig {
    /// Sandbox section — language runtimes, setup commands, environment.
    #[serde(default)]
    pub sandbox: Option<UserSandboxSection>,

    /// Docker services to run alongside the session.
    #[serde(default)]
    pub docker: Option<DockerSection>,

    /// Named test suites that can be triggered from the TUI.
    #[serde(default)]
    pub tests: Option<Vec<TestSuite>>,

    /// Instructions injected into Claude's context.
    #[serde(default)]
    pub instructions: Option<InstructionsSection>,
}

/// The `sandbox:` section of the user config.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UserSandboxSection {
    /// Whether to auto-detect language runtimes. Default: false.
    #[serde(default)]
    pub auto_detect: bool,

    /// Explicit list of language runtimes (e.g. `["java", "python", "node"]`).
    #[serde(default)]
    pub languages: Vec<String>,

    /// Path to a Python virtual environment, relative to repo root.
    #[serde(default)]
    pub venv_path: Option<String>,

    /// Shell commands to run during setup (in order).
    #[serde(default)]
    pub setup: Vec<String>,

    /// Extra environment variables for the sandbox.
    #[serde(default)]
    pub env: BTreeMap<String, String>,

    /// Convenience shorthand for `JAVA_HOME` (also set in `env` automatically).
    #[serde(default)]
    pub java_home: Option<String>,
}

/// The `docker:` section — a compose file and its services with port mappings.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DockerSection {
    /// Path to the docker-compose file, relative to repo root.
    pub compose_file: String,

    /// Services to manage, keyed by compose service name.
    #[serde(default)]
    pub services: BTreeMap<String, DockerServiceDef>,
}

/// Per-service definition inside `docker.services`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DockerServiceDef {
    /// Host ports this service exposes (original/default ports).
    #[serde(default)]
    pub ports: Vec<u16>,
}

/// A named test suite that can be run from the TUI.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TestSuite {
    /// Human-readable name shown in the UI.
    pub name: String,

    /// Shell command to execute.
    pub command: String,

    /// SF Symbol or icon identifier for the UI.
    #[serde(default)]
    pub icon: Option<String>,

    /// Timeout in seconds. Default: 300.
    #[serde(default = "default_test_timeout")]
    pub timeout: u64,
}

fn default_test_timeout() -> u64 {
    300
}

/// The `instructions:` section — context injected into Claude.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct InstructionsSection {
    /// Instructions applied to all sessions for this project.
    #[serde(default)]
    pub all: Option<String>,
}

/// Load a user-facing `sandbox.yaml` from a repository root.
///
/// Returns `None` if the file does not exist.
pub fn load_user_sandbox(repo_path: &Path) -> Option<UserSandboxConfig> {
    let path = repo_path.join("sandbox.yaml");
    if !path.exists() {
        return None;
    }
    let content = fs::read_to_string(&path).ok()?;
    serde_yaml::from_str(&content).ok()
}

// ── Port Allocation Engine ──────────────────────────────────────────────────

/// Allocated port mappings for a session's Docker services.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortMapping {
    /// The base offset in the ephemeral port range used for this session.
    pub base_offset: u16,

    /// Service name -> list of (`original_port`, `remapped_port`) tuples.
    pub mappings: BTreeMap<String, Vec<(u16, u16)>>,
}

/// Minimum base offset for port allocation.
const PORT_RANGE_MIN: u16 = 10_000;

/// Maximum base offset for port allocation.
const PORT_RANGE_MAX: u16 = 60_000;

/// Step size between allocation slots.
const PORT_RANGE_STEP: u16 = 100;

/// Maximum number of attempts to find a free port range.
const MAX_ALLOCATION_ATTEMPTS: u16 = 500;

impl PortMapping {
    /// Allocate ports for a session. Hashes `session_id` to pick a base offset
    /// in the range `10000..60000` (stepping by 100), then remaps each service
    /// port as `base_offset + (original_port % 100)`.
    ///
    /// If a collision is detected, tries the next 100-offset until a free range
    /// is found. Writes the result to `~/.claustre/ports/{session_id}.json`.
    pub fn allocate(docker: &DockerSection, session_id: &str) -> Result<Self> {
        let mut hasher = DefaultHasher::new();
        session_id.hash(&mut hasher);
        let hash = hasher.finish();

        let num_slots = u64::from((PORT_RANGE_MAX - PORT_RANGE_MIN) / PORT_RANGE_STEP);
        let start_slot = hash % num_slots;

        for attempt in 0..u64::from(MAX_ALLOCATION_ATTEMPTS) {
            let slot = (start_slot + attempt) % num_slots;
            let base =
                PORT_RANGE_MIN + u16::try_from(slot).expect("slot fits u16") * PORT_RANGE_STEP;

            let mut mappings = BTreeMap::new();
            let mut all_available = true;

            for (service_name, service_def) in &docker.services {
                let mut port_pairs = Vec::with_capacity(service_def.ports.len());
                for &original in &service_def.ports {
                    let remapped = base + (original % 100);
                    if !Self::port_available(remapped) {
                        all_available = false;
                        break;
                    }
                    port_pairs.push((original, remapped));
                }
                if !all_available {
                    break;
                }
                mappings.insert(service_name.clone(), port_pairs);
            }

            if all_available {
                let mapping = Self {
                    base_offset: base,
                    mappings,
                };
                mapping.persist(session_id)?;
                return Ok(mapping);
            }
        }

        bail!(
            "failed to allocate ports for session '{session_id}' after {MAX_ALLOCATION_ATTEMPTS} attempts"
        )
    }

    /// Check whether a port is available by attempting to bind to it.
    fn port_available(port: u16) -> bool {
        TcpListener::bind(("127.0.0.1", port)).is_ok()
    }

    /// Persist allocated ports to `~/.claustre/ports/{session_id}.json`.
    fn persist(&self, session_id: &str) -> Result<()> {
        let dir = config::ports_dir()?;
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create ports directory {}", dir.display()))?;
        let path = dir.join(format!("{session_id}.json"));
        let json =
            serde_json::to_string_pretty(self).context("failed to serialize port mapping")?;
        fs::write(&path, json)
            .with_context(|| format!("failed to write port mapping to {}", path.display()))?;
        Ok(())
    }

    /// Remove the persisted port allocation file for a session.
    pub fn deallocate(session_id: &str) -> Result<()> {
        let path = config::ports_dir()?.join(format!("{session_id}.json"));
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove port mapping {}", path.display()))?;
        }
        Ok(())
    }

    /// Load a previously persisted port mapping for a session.
    pub fn load(session_id: &str) -> Result<Option<Self>> {
        let path = config::ports_dir()?.join(format!("{session_id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read port mapping from {}", path.display()))?;
        let mapping: Self = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse port mapping from {}", path.display()))?;
        Ok(Some(mapping))
    }

    /// Generate a `docker-compose.override.yml` string with remapped port bindings.
    ///
    /// The override maps each service's original container port to the
    /// session-specific host port.
    pub fn generate_compose_override(&self) -> String {
        let mut lines = Vec::new();
        lines.push("services:".to_string());

        for (service_name, port_pairs) in &self.mappings {
            if port_pairs.is_empty() {
                continue;
            }
            lines.push(format!("  {service_name}:"));
            lines.push("    ports:".to_string());
            for &(original, remapped) in port_pairs {
                lines.push(format!("      - \"{remapped}:{original}\""));
            }
        }

        lines.join("\n") + "\n"
    }

    /// Generate a human-readable summary for Claude's context injection.
    ///
    /// Lists each service with its remapped port bindings so Claude knows
    /// which ports to use when connecting to services.
    pub fn context_summary(&self) -> String {
        let mut parts = Vec::new();
        parts.push("Port mappings for this session:".to_string());

        for (service_name, port_pairs) in &self.mappings {
            for &(original, remapped) in port_pairs {
                parts.push(format!(
                    "  {service_name}: localhost:{remapped} -> container:{original}"
                ));
            }
        }

        parts.join("\n")
    }
}

fn stop_command_service(pid: u32, stop_command: Option<&str>, cwd: PathBuf) -> Result<()> {
    if let Some(stop_command) = stop_command {
        return run_shell_command(stop_command, cwd, &BTreeMap::new(), None);
    }

    let pid_i32 = i32::try_from(pid).context("command service pid out of range")?;
    // SAFETY: `kill` is the intended libc API for signaling child processes on Unix.
    let term_ok = unsafe { libc::kill(pid_i32, libc::SIGTERM) == 0 };
    anyhow::ensure!(term_ok, "failed to send SIGTERM to pid {pid}");
    thread::sleep(Duration::from_secs(1));
    // SAFETY: `kill` with signal 0 is the POSIX way to test if a process exists.
    let still_running = unsafe { libc::kill(pid_i32, 0) == 0 };
    if still_running {
        // SAFETY: escalates to SIGKILL only if the process ignored SIGTERM.
        let kill_ok = unsafe { libc::kill(pid_i32, libc::SIGKILL) == 0 };
        anyhow::ensure!(kill_ok, "failed to send SIGKILL to pid {pid}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_profile_defaults_without_explicit_profiles() {
        let sandbox = SandboxConfig {
            version: 1,
            build: vec![BuildStep {
                name: "deps".to_string(),
                run: "echo deps".to_string(),
                cwd: None,
                env: BTreeMap::new(),
                required_on_launch: true,
            }],
            services: vec![ServiceConfig {
                name: "web".to_string(),
                kind: RuntimeServiceKind::Command,
                cwd: None,
                env: BTreeMap::new(),
                auto_start: true,
                persistent: false,
                ready: None,
                stop: None,
                run: Some("python -m http.server".to_string()),
                file: None,
                service: None,
                project_name: None,
            }],
            checks: vec![],
            env: BTreeMap::new(),
            profiles: BTreeMap::new(),
        };

        let resolved = resolve_profile(&sandbox, None).unwrap();
        assert_eq!(resolved.build_steps.len(), 1);
        assert_eq!(resolved.services.len(), 1);
        assert!(resolved.build_on_launch);
    }

    #[test]
    fn resolve_profile_uses_named_profile() {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "default".to_string(),
            RuntimeProfile {
                build: vec!["deps".to_string()],
                services: vec!["web".to_string()],
                checks: vec![],
                build_on_launch: true,
                default_provider: None,
                default_inspector_tab: None,
            },
        );
        let sandbox = SandboxConfig {
            version: 1,
            env: BTreeMap::new(),
            build: vec![BuildStep {
                name: "deps".to_string(),
                run: "echo deps".to_string(),
                cwd: None,
                env: BTreeMap::new(),
                required_on_launch: true,
            }],
            services: vec![ServiceConfig {
                name: "web".to_string(),
                kind: RuntimeServiceKind::Command,
                cwd: None,
                env: BTreeMap::new(),
                auto_start: false,
                persistent: false,
                ready: None,
                stop: None,
                run: Some("python -m http.server".to_string()),
                file: None,
                service: None,
                project_name: None,
            }],
            checks: vec![],
            profiles,
        };

        let resolved = resolve_profile(&sandbox, Some("default")).unwrap();
        assert_eq!(resolved.name, "default");
        assert_eq!(resolved.build_steps.len(), 1);
        assert_eq!(resolved.services.len(), 1);
    }

    // ── User sandbox YAML parsing tests ─────────────────────────────────

    #[test]
    fn parse_full_user_sandbox_yaml() {
        let yaml = r#"
sandbox:
  auto_detect: true
  languages: [java, python, node]
  venv_path: env
  setup:
    - "source env/bin/activate"
    - "make install_dev generate"
  java_home: "$HOME/.sdkman/candidates/java/current"
  env:
    JAVA_HOME: "$HOME/.sdkman/candidates/java/current"

docker:
  compose_file: docker/development/docker-compose.yml
  services:
    mysql:
      ports: [3306]
    elasticsearch:
      ports: [9200, 9300]

tests:
  - name: "Java Unit Tests"
    command: "mvn test"
    icon: "cup.and.saucer"
    timeout: 600

instructions:
  all: |
    Context for Claude...
"#;
        let config: UserSandboxConfig = serde_yaml::from_str(yaml).unwrap();

        let sandbox = config.sandbox.unwrap();
        assert!(sandbox.auto_detect);
        assert_eq!(sandbox.languages, vec!["java", "python", "node"]);
        assert_eq!(sandbox.venv_path.as_deref(), Some("env"));
        assert_eq!(sandbox.setup.len(), 2);
        assert_eq!(
            sandbox.java_home.as_deref(),
            Some("$HOME/.sdkman/candidates/java/current")
        );
        assert!(sandbox.env.contains_key("JAVA_HOME"));

        let docker = config.docker.unwrap();
        assert_eq!(docker.compose_file, "docker/development/docker-compose.yml");
        assert_eq!(docker.services.len(), 2);
        assert_eq!(docker.services["mysql"].ports, vec![3306]);
        assert_eq!(docker.services["elasticsearch"].ports, vec![9200, 9300]);

        let tests = config.tests.unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].name, "Java Unit Tests");
        assert_eq!(tests[0].command, "mvn test");
        assert_eq!(tests[0].icon.as_deref(), Some("cup.and.saucer"));
        assert_eq!(tests[0].timeout, 600);

        let instructions = config.instructions.unwrap();
        assert!(instructions.all.unwrap().contains("Context for Claude"));
    }

    #[test]
    fn parse_minimal_user_sandbox_yaml() {
        let yaml = "{}";
        let config: UserSandboxConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.sandbox.is_none());
        assert!(config.docker.is_none());
        assert!(config.tests.is_none());
        assert!(config.instructions.is_none());
    }

    #[test]
    fn parse_sandbox_section_only() {
        let yaml = r#"
sandbox:
  languages: [rust]
  setup:
    - "cargo build"
"#;
        let config: UserSandboxConfig = serde_yaml::from_str(yaml).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert!(!sandbox.auto_detect);
        assert_eq!(sandbox.languages, vec!["rust"]);
        assert_eq!(sandbox.setup, vec!["cargo build"]);
        assert!(sandbox.venv_path.is_none());
        assert!(sandbox.java_home.is_none());
    }

    #[test]
    fn parse_docker_section_only() {
        let yaml = r#"
docker:
  compose_file: docker-compose.yml
  services:
    redis:
      ports: [6379]
    postgres:
      ports: [5432]
"#;
        let config: UserSandboxConfig = serde_yaml::from_str(yaml).unwrap();
        let docker = config.docker.unwrap();
        assert_eq!(docker.compose_file, "docker-compose.yml");
        assert_eq!(docker.services.len(), 2);
        assert_eq!(docker.services["redis"].ports, vec![6379]);
        assert_eq!(docker.services["postgres"].ports, vec![5432]);
    }

    #[test]
    fn parse_tests_default_timeout() {
        let yaml = r#"
tests:
  - name: "Quick test"
    command: "echo ok"
"#;
        let config: UserSandboxConfig = serde_yaml::from_str(yaml).unwrap();
        let tests = config.tests.unwrap();
        assert_eq!(tests[0].timeout, 300);
        assert!(tests[0].icon.is_none());
    }

    #[test]
    fn load_user_sandbox_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_user_sandbox(dir.path()).is_none());
    }

    #[test]
    fn load_user_sandbox_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = r#"
sandbox:
  languages: [go]
docker:
  compose_file: docker-compose.yml
  services:
    db:
      ports: [5432]
"#;
        fs::write(dir.path().join("sandbox.yaml"), yaml).unwrap();
        let config = load_user_sandbox(dir.path()).unwrap();
        let sandbox = config.sandbox.unwrap();
        assert_eq!(sandbox.languages, vec!["go"]);
    }

    // ── Port allocation tests ───────────────────────────────────────────

    #[test]
    fn port_allocation_deterministic_base() {
        let docker = DockerSection {
            compose_file: "docker-compose.yml".to_string(),
            services: BTreeMap::new(),
        };
        // With no services, allocation should succeed trivially
        let mapping = PortMapping::allocate(&docker, "test-session-1").unwrap();
        assert!(mapping.base_offset >= PORT_RANGE_MIN);
        assert!(mapping.base_offset < PORT_RANGE_MAX);
        assert!(mapping.mappings.is_empty());

        // Clean up
        let _ = PortMapping::deallocate("test-session-1");
    }

    #[test]
    fn port_allocation_remaps_ports() {
        let mut services = BTreeMap::new();
        services.insert("mysql".to_string(), DockerServiceDef { ports: vec![3306] });
        services.insert("redis".to_string(), DockerServiceDef { ports: vec![6379] });
        let docker = DockerSection {
            compose_file: "docker-compose.yml".to_string(),
            services,
        };

        let mapping = PortMapping::allocate(&docker, "test-session-ports").unwrap();
        assert_eq!(mapping.mappings.len(), 2);

        let mysql_ports = &mapping.mappings["mysql"];
        assert_eq!(mysql_ports.len(), 1);
        assert_eq!(mysql_ports[0].0, 3306);
        // Remapped = base + (3306 % 100) = base + 6
        assert_eq!(mysql_ports[0].1, mapping.base_offset + 6);

        let redis_ports = &mapping.mappings["redis"];
        assert_eq!(redis_ports.len(), 1);
        assert_eq!(redis_ports[0].0, 6379);
        // Remapped = base + (6379 % 100) = base + 79
        assert_eq!(redis_ports[0].1, mapping.base_offset + 79);

        // Clean up
        let _ = PortMapping::deallocate("test-session-ports");
    }

    #[test]
    fn port_allocation_multi_port_service() {
        let mut services = BTreeMap::new();
        services.insert(
            "elasticsearch".to_string(),
            DockerServiceDef {
                ports: vec![9200, 9300],
            },
        );
        let docker = DockerSection {
            compose_file: "docker-compose.yml".to_string(),
            services,
        };

        let mapping = PortMapping::allocate(&docker, "test-session-multi").unwrap();
        let es_ports = &mapping.mappings["elasticsearch"];
        assert_eq!(es_ports.len(), 2);
        assert_eq!(es_ports[0].0, 9200);
        assert_eq!(es_ports[0].1, mapping.base_offset + 0); // 9200 % 100 = 0
        assert_eq!(es_ports[1].0, 9300);
        assert_eq!(es_ports[1].1, mapping.base_offset + 0); // 9300 % 100 = 0

        // Clean up
        let _ = PortMapping::deallocate("test-session-multi");
    }

    #[test]
    fn port_available_check() {
        // Port 0 asks the OS to pick an available port
        assert!(PortMapping::port_available(0));
        // A very high port should generally be available in test
        assert!(PortMapping::port_available(59999));
    }

    #[test]
    fn port_mapping_persist_and_load() {
        let mapping = PortMapping {
            base_offset: 15_000,
            mappings: {
                let mut m = BTreeMap::new();
                m.insert("db".to_string(), vec![(5432, 15_032)]);
                m
            },
        };
        mapping.persist("test-persist-load").unwrap();

        let loaded = PortMapping::load("test-persist-load").unwrap().unwrap();
        assert_eq!(loaded.base_offset, 15_000);
        assert_eq!(loaded.mappings["db"], vec![(5432, 15_032)]);

        PortMapping::deallocate("test-persist-load").unwrap();
        assert!(PortMapping::load("test-persist-load").unwrap().is_none());
    }

    #[test]
    fn port_mapping_load_nonexistent() {
        let loaded = PortMapping::load("nonexistent-session-xyz").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn port_mapping_deallocate_nonexistent() {
        // Should not error when file doesn't exist
        PortMapping::deallocate("nonexistent-session-abc").unwrap();
    }

    // ── Compose override generation tests ───────────────────────────────

    #[test]
    fn generate_compose_override_single_service() {
        let mut mappings = BTreeMap::new();
        mappings.insert("mysql".to_string(), vec![(3306, 13306)]);
        let mapping = PortMapping {
            base_offset: 13300,
            mappings,
        };

        let override_yaml = mapping.generate_compose_override();
        assert!(override_yaml.contains("services:"));
        assert!(override_yaml.contains("  mysql:"));
        assert!(override_yaml.contains("    ports:"));
        assert!(override_yaml.contains("      - \"13306:3306\""));
    }

    #[test]
    fn generate_compose_override_multiple_services() {
        let mut mappings = BTreeMap::new();
        mappings.insert("mysql".to_string(), vec![(3306, 13306)]);
        mappings.insert(
            "elasticsearch".to_string(),
            vec![(9200, 13200), (9300, 13300)],
        );
        let mapping = PortMapping {
            base_offset: 13200,
            mappings,
        };

        let override_yaml = mapping.generate_compose_override();
        assert!(override_yaml.contains("  elasticsearch:"));
        assert!(override_yaml.contains("      - \"13200:9200\""));
        assert!(override_yaml.contains("      - \"13300:9300\""));
        assert!(override_yaml.contains("  mysql:"));
        assert!(override_yaml.contains("      - \"13306:3306\""));
    }

    #[test]
    fn generate_compose_override_empty_ports_skipped() {
        let mut mappings = BTreeMap::new();
        mappings.insert("empty_service".to_string(), vec![]);
        mappings.insert("real_service".to_string(), vec![(8080, 18080)]);
        let mapping = PortMapping {
            base_offset: 18000,
            mappings,
        };

        let override_yaml = mapping.generate_compose_override();
        assert!(!override_yaml.contains("empty_service"));
        assert!(override_yaml.contains("  real_service:"));
    }

    #[test]
    fn context_summary_format() {
        let mut mappings = BTreeMap::new();
        mappings.insert("mysql".to_string(), vec![(3306, 13306)]);
        mappings.insert("redis".to_string(), vec![(6379, 13379)]);
        let mapping = PortMapping {
            base_offset: 13300,
            mappings,
        };

        let summary = mapping.context_summary();
        assert!(summary.contains("Port mappings for this session:"));
        assert!(summary.contains("mysql: localhost:13306 -> container:3306"));
        assert!(summary.contains("redis: localhost:13379 -> container:6379"));
    }

    #[test]
    fn context_summary_empty_mappings() {
        let mapping = PortMapping {
            base_offset: 10000,
            mappings: BTreeMap::new(),
        };
        let summary = mapping.context_summary();
        assert_eq!(summary, "Port mappings for this session:");
    }
}
