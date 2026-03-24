//! GitHub App device-flow authentication and local session persistence.
//!
//! This module provides the first usable GitHub App auth path for claustre:
//! local device flow, persisted session tokens, and optional token refresh for
//! expiring user-to-server access tokens.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use crate::config::GithubAppConfig;

const GITHUB_OAUTH_BASE: &str = "https://github.com/login/oauth";
const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_VERSION: &str = "2022-11-28";
const ACCESS_TOKEN_REFRESH_SKEW_SECS: i64 = 300;
const DEFAULT_GITHUB_APP_CLIENT_ID: Option<&str> = option_env!("CLAUSTRE_GITHUB_APP_CLIENT_ID");
const DEFAULT_GITHUB_APP_SLUG: Option<&str> = option_env!("CLAUSTRE_GITHUB_APP_SLUG");
const DEFAULT_GITHUB_APP_INSTALL_URL: Option<&str> = option_env!("CLAUSTRE_GITHUB_APP_INSTALL_URL");
const DEFAULT_GITHUB_APP_RELAY_URL: Option<&str> = option_env!("CLAUSTRE_GITHUB_APP_RELAY_URL");

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitHubAppSession {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub user_login: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub refresh_token_expires_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GitHubAppStatus {
    pub configured: bool,
    pub authenticated: bool,
    pub client_id: Option<String>,
    pub app_slug: Option<String>,
    pub install_url: Option<String>,
    pub relay_url: Option<String>,
    pub default_installation_id: Option<String>,
    pub default_project_id: Option<String>,
    pub user_login: Option<String>,
    pub user_name: Option<String>,
    pub expires_at: Option<String>,
    pub session_path: Option<String>,
    pub custom_app_configured: bool,
    pub gh_authenticated: bool,
    pub gh_user_login: Option<String>,
    pub gh_scopes: Vec<String>,
    pub gh_has_project_scope: bool,
    pub app_has_project_scope: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubViewer {
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubInstallationAccount {
    pub login: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubInstallation {
    pub id: i64,
    pub account: GitHubInstallationAccount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubDevicePrompt {
    #[serde(default)]
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
    pub expires_at: String,
}

#[derive(Debug, Clone)]
pub enum GitHubAuthMessage {
    Prompt(GitHubDevicePrompt),
    Success(GitHubAppSession),
    CliSuccess(Option<String>),
    Failed(String),
    Disconnected,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    #[serde(default = "default_device_poll_interval")]
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token_expires_in: Option<u64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

fn default_device_poll_interval() -> u64 {
    5
}

fn github_app_session_path() -> Result<PathBuf> {
    Ok(crate::config::base_dir()?.join("github-app-session.json"))
}

pub fn load_session() -> Result<Option<GitHubAppSession>> {
    let path = github_app_session_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let session = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(session))
}

pub fn save_session(session: &GitHubAppSession) -> Result<()> {
    let path = github_app_session_path()?;
    let json = serde_json::to_string_pretty(session).context("failed to serialize session")?;
    fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn clear_session() -> Result<()> {
    let path = github_app_session_path()?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

pub fn local_status(config: &GithubAppConfig) -> Result<GitHubAppStatus> {
    let session = load_session()?;
    let gh_status = github_cli_status().unwrap_or_default();
    let session_path = github_app_session_path()
        .ok()
        .map(|path| path.display().to_string());
    let app_has_project_scope = session
        .as_ref()
        .and_then(|session| session.scope.as_deref())
        .is_some_and(scope_string_has_project_access);
    Ok(GitHubAppStatus {
        configured: app_auth_available(config),
        authenticated: session.is_some(),
        client_id: resolved_client_id(config),
        app_slug: resolved_app_slug(config),
        install_url: install_url(config),
        relay_url: resolved_relay_url(config),
        default_installation_id: config.default_installation_id.clone(),
        default_project_id: config.default_project_id.clone(),
        user_login: session
            .as_ref()
            .and_then(|session| session.user_login.clone()),
        user_name: session
            .as_ref()
            .and_then(|session| session.user_name.clone()),
        expires_at: session
            .as_ref()
            .and_then(|session| session.expires_at.clone()),
        session_path,
        custom_app_configured: custom_app_configured(config),
        gh_authenticated: gh_status.user_login.is_some(),
        gh_user_login: gh_status.user_login,
        gh_scopes: gh_status.scopes.clone(),
        gh_has_project_scope: scopes_have_project_access(&gh_status.scopes),
        app_has_project_scope,
    })
}

pub fn app_auth_available(config: &GithubAppConfig) -> bool {
    resolved_client_id(config).is_some()
}

pub fn custom_app_configured(config: &GithubAppConfig) -> bool {
    [
        config.client_id.as_deref(),
        config.app_slug.as_deref(),
        config.install_url.as_deref(),
        config.relay_url.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .any(|value| !value.is_empty())
}

pub fn resolved_client_id(config: &GithubAppConfig) -> Option<String> {
    resolve_optional_config(
        config.client_id.as_deref(),
        "CLAUSTRE_GITHUB_APP_CLIENT_ID",
        DEFAULT_GITHUB_APP_CLIENT_ID,
    )
}

pub fn resolved_app_slug(config: &GithubAppConfig) -> Option<String> {
    resolve_optional_config(
        config.app_slug.as_deref(),
        "CLAUSTRE_GITHUB_APP_SLUG",
        DEFAULT_GITHUB_APP_SLUG,
    )
}

pub fn resolved_relay_url(config: &GithubAppConfig) -> Option<String> {
    resolve_optional_config(
        config.relay_url.as_deref(),
        "CLAUSTRE_GITHUB_APP_RELAY_URL",
        DEFAULT_GITHUB_APP_RELAY_URL,
    )
}

pub fn install_url(config: &GithubAppConfig) -> Option<String> {
    resolve_optional_config(
        config.install_url.as_deref(),
        "CLAUSTRE_GITHUB_APP_INSTALL_URL",
        DEFAULT_GITHUB_APP_INSTALL_URL,
    )
    .or_else(|| {
        resolved_app_slug(config)
            .map(|slug| format!("https://github.com/apps/{slug}/installations/new"))
    })
}

pub fn open_in_browser(url: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = Command::new(opener)
        .arg(url)
        .status()
        .with_context(|| format!("failed to launch browser opener `{opener}`"))?;
    anyhow::ensure!(status.success(), "browser opener exited unsuccessfully");
    Ok(())
}

pub fn authenticate_device_flow<F>(
    config: &GithubAppConfig,
    mut on_event: F,
) -> Result<GitHubAppSession>
where
    F: FnMut(GitHubAuthMessage),
{
    let client_id = resolved_client_id(config).context("GitHub App client_id is not configured")?;

    let start = start_device_flow(&client_id)?;
    on_event(GitHubAuthMessage::Prompt(start.clone()));

    if let Some(url) = start
        .verification_uri_complete
        .as_deref()
        .or(Some(start.verification_uri.as_str()))
    {
        let _ = open_in_browser(url);
    }

    let mut poll_interval = start.interval.max(1);
    let deadline = Instant::now() + Duration::from_secs(start.expires_in);

    loop {
        if Instant::now() >= deadline {
            anyhow::bail!("GitHub device authorization expired before completion");
        }
        std::thread::sleep(Duration::from_secs(poll_interval));

        let token_response = request_access_token(&client_id, &start.device_code)?;
        match token_response.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                poll_interval = poll_interval.saturating_add(5);
                continue;
            }
            Some("expired_token") => anyhow::bail!("GitHub device code expired"),
            Some(error) => {
                let description = token_response
                    .error_description
                    .as_deref()
                    .unwrap_or("unknown authorization failure");
                anyhow::bail!("{error}: {description}");
            }
            None => {}
        }

        let access_token = token_response
            .access_token
            .context("GitHub did not return an access token")?;
        let viewer = fetch_viewer(&access_token)?;
        let session = GitHubAppSession {
            access_token,
            refresh_token: token_response.refresh_token,
            token_type: token_response.token_type,
            scope: token_response.scope,
            user_login: Some(viewer.login),
            user_name: viewer.name,
            expires_at: token_response.expires_in.map(rfc3339_after_seconds),
            refresh_token_expires_at: token_response
                .refresh_token_expires_in
                .map(rfc3339_after_seconds),
            created_at: Some(Utc::now().to_rfc3339()),
        };
        save_session(&session)?;
        on_event(GitHubAuthMessage::Success(session.clone()));
        return Ok(session);
    }
}

pub fn ensure_access_token(config: &GithubAppConfig) -> Result<Option<String>> {
    let Some(mut session) = load_session()? else {
        return Ok(None);
    };

    if session_needs_refresh(&session) {
        let Some(client_id) = resolved_client_id(config) else {
            return Ok(None);
        };

        let Some(refresh_token) = session.refresh_token.as_deref() else {
            return Ok(None);
        };

        let refreshed = refresh_access_token(&client_id, refresh_token)?;
        let viewer = fetch_viewer(
            refreshed
                .access_token
                .as_deref()
                .context("GitHub refresh response missing access token")?,
        )
        .ok();

        session.access_token = refreshed
            .access_token
            .context("GitHub refresh response missing access token")?;
        session.refresh_token = refreshed.refresh_token;
        session.token_type = refreshed.token_type;
        session.scope = refreshed.scope;
        session.expires_at = refreshed.expires_in.map(rfc3339_after_seconds);
        session.refresh_token_expires_at = refreshed
            .refresh_token_expires_in
            .map(rfc3339_after_seconds);
        session.created_at = Some(Utc::now().to_rfc3339());
        if let Some(viewer) = viewer {
            session.user_login = Some(viewer.login);
            session.user_name = viewer.name;
        }
        save_session(&session)?;
    }

    Ok(Some(session.access_token))
}

pub fn github_cli_user() -> Result<Option<String>> {
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .context("failed to run `gh api user`")?;
    if !output.status.success() {
        return Ok(None);
    }

    let login = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!login.is_empty()).then_some(login))
}

#[derive(Debug, Clone, Default)]
struct GitHubCliStatus {
    user_login: Option<String>,
    scopes: Vec<String>,
}

pub fn scopes_have_project_access(scopes: &[String]) -> bool {
    scopes
        .iter()
        .map(|scope| scope.trim())
        .any(|scope| matches!(scope, "project" | "read:project"))
}

fn scope_string_has_project_access(scope: &str) -> bool {
    scopes_have_project_access(&parse_scope_list(scope))
}

fn parse_scope_list(scopes: &str) -> Vec<String> {
    scopes
        .split(',')
        .map(|scope| scope.trim().trim_matches('\'').trim_matches('"'))
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect()
}

fn github_cli_status() -> Result<GitHubCliStatus> {
    let output = Command::new("gh")
        .args(["auth", "status", "--hostname", "github.com"])
        .output()
        .context("failed to run `gh auth status --hostname github.com`")?;
    if !output.status.success() {
        return Ok(GitHubCliStatus::default());
    }

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let user_login = combined
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("✓ Logged in to github.com account ")
                .or_else(|| {
                    line.trim()
                        .strip_prefix("- Logged in to github.com account ")
                })
                .map(|rest| {
                    rest.split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .trim_matches('(')
                        .to_string()
                })
        })
        .filter(|login| !login.is_empty());

    let scopes = combined
        .lines()
        .find_map(|line| line.trim().strip_prefix("- Token scopes: "))
        .map(parse_scope_list)
        .unwrap_or_default();

    Ok(GitHubCliStatus { user_login, scopes })
}

pub fn github_cli_token() -> Result<Option<String>> {
    let output = Command::new("gh")
        .args(["auth", "token"])
        .output()
        .context("failed to run `gh auth token`")?;
    if !output.status.success() {
        return Ok(None);
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!token.is_empty()).then_some(token))
}

pub fn github_cli_connect() -> Result<Option<String>> {
    if github_cli_user()?.is_some() {
        run_github_cli_command(&[
            "auth",
            "refresh",
            "--hostname",
            "github.com",
            "--scopes",
            "repo,read:org,read:project,project",
        ])?;
    } else {
        run_github_cli_command(&[
            "auth",
            "login",
            "--hostname",
            "github.com",
            "--web",
            "--git-protocol",
            "https",
            "--skip-ssh-key",
            "--scopes",
            "repo,read:org,read:project,project",
        ])?;
    }

    github_cli_user()
}

pub fn github_cli_disconnect() -> Result<()> {
    run_github_cli_command(&["auth", "logout", "-h", "github.com"])?;
    Ok(())
}

pub fn fetch_viewer(token: &str) -> Result<GitHubViewer> {
    github_api_get("user", token)
}

pub fn list_installations(token: &str) -> Result<Vec<GitHubInstallation>> {
    #[derive(Debug, Deserialize)]
    struct InstallationEnvelope {
        #[serde(default)]
        installations: Vec<GitHubInstallation>,
    }

    let envelope: InstallationEnvelope = github_api_get("user/installations", token)?;
    Ok(envelope.installations)
}

fn start_device_flow(client_id: &str) -> Result<GitHubDevicePrompt> {
    let response: DeviceCodeResponse = oauth_post_form(
        "device/code",
        &[
            ("client_id", client_id),
            ("scope", "repo read:org read:project project"),
        ],
    )?;
    Ok(GitHubDevicePrompt {
        device_code: response.device_code,
        user_code: response.user_code,
        verification_uri: response.verification_uri,
        verification_uri_complete: response.verification_uri_complete,
        expires_in: response.expires_in,
        interval: response.interval.max(1),
        expires_at: rfc3339_after_seconds(response.expires_in),
    })
}

fn request_access_token(client_id: &str, device_code: &str) -> Result<AccessTokenResponse> {
    oauth_post_form(
        "access_token",
        &[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
    )
}

fn refresh_access_token(client_id: &str, refresh_token: &str) -> Result<AccessTokenResponse> {
    oauth_post_form(
        "access_token",
        &[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
}

fn oauth_post_form<T>(path: &str, body: &[(&str, &str)]) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let url = format!("{GITHUB_OAUTH_BASE}/{path}");
    let response = ureq::post(&url)
        .set("Accept", "application/json")
        .set("User-Agent", "claustre")
        .send_form(body)
        .map_err(http_error)?;
    response
        .into_json()
        .context("failed to parse GitHub OAuth response")
}

fn resolve_optional_config(
    configured: Option<&str>,
    env_key: &str,
    compiled_default: Option<&str>,
) -> Option<String> {
    configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(env_key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .or_else(|| {
            compiled_default
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
}

fn run_github_cli_command(args: &[&str]) -> Result<()> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `gh {}`", args.join(" ")))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    anyhow::bail!("{}", if stderr.is_empty() { stdout } else { stderr });
}

fn github_api_get<T>(path: &str, token: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let url = format!("{GITHUB_API_BASE}/{path}");
    let response = ureq::get(&url)
        .set("Accept", "application/vnd.github.full+json")
        .set("Authorization", &format!("Bearer {token}"))
        .set("User-Agent", "claustre")
        .set("X-GitHub-Api-Version", GITHUB_API_VERSION)
        .call()
        .map_err(http_error)?;
    response
        .into_json()
        .context("failed to parse GitHub API response")
}

fn http_error(error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            anyhow::anyhow!("GitHub request failed ({code}): {}", body.trim())
        }
        ureq::Error::Transport(error) => anyhow::anyhow!("GitHub request failed: {error}"),
    }
}

fn session_needs_refresh(session: &GitHubAppSession) -> bool {
    let Some(expires_at) = session.expires_at.as_deref() else {
        return false;
    };
    let Ok(expires_at) = DateTime::parse_from_rfc3339(expires_at) else {
        return false;
    };
    Utc::now() + ChronoDuration::seconds(ACCESS_TOKEN_REFRESH_SKEW_SECS)
        >= expires_at.with_timezone(&Utc)
}

fn rfc3339_after_seconds(seconds: u64) -> String {
    let seconds = i64::try_from(seconds).unwrap_or(i64::MAX);
    (Utc::now() + ChronoDuration::seconds(seconds)).to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_url_prefers_explicit_url() {
        let config = GithubAppConfig {
            install_url: Some("https://example.com/install".to_string()),
            app_slug: Some("claustre".to_string()),
            ..GithubAppConfig::default()
        };
        assert_eq!(
            install_url(&config).as_deref(),
            Some("https://example.com/install")
        );
    }

    #[test]
    fn install_url_uses_app_slug_when_explicit_url_missing() {
        let config = GithubAppConfig {
            app_slug: Some("claustre".to_string()),
            ..GithubAppConfig::default()
        };
        assert_eq!(
            install_url(&config).as_deref(),
            Some("https://github.com/apps/claustre/installations/new")
        );
    }

    #[test]
    fn session_refresh_detects_imminent_expiry() {
        let session = GitHubAppSession {
            access_token: "token".to_string(),
            expires_at: Some((Utc::now() + ChronoDuration::minutes(2)).to_rfc3339()),
            ..GitHubAppSession::default()
        };
        assert!(session_needs_refresh(&session));
    }
}
