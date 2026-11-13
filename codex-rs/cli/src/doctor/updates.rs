//! Checks Web2Codex's GitHub release channel without invoking package managers.

use std::path::Path;
use std::time::Duration;

use codex_core::config::Config;
use codex_http_client::ClientRouteClass;
use codex_http_client::RouteAwareClientPool;
use codex_install_context::InstallContext;
use codex_install_context::InstallMethod;
use http::Method;
use serde::Deserialize;

use super::CheckStatus;
use super::DoctorCheck;
use super::doctor_install_context;

const MAX_VERSION_RESPONSE_BYTES: usize = 1024 * 1024;
const VERSION_FILE_NAME: &str = "version.json";

pub(super) async fn updates_check(config: &Config) -> DoctorCheck {
    let current_exe = std::env::current_exe().ok();
    let install_context = doctor_install_context(current_exe.as_deref());
    let mut details = vec![
        format!(
            "check for update on startup: {}",
            config.check_for_update_on_startup
        ),
        format!("update action: {}", update_action_label(&install_context)),
    ];
    push_cached_version_details(&mut details, &config.codex_home.join(VERSION_FILE_NAME));

    let client = RouteAwareClientPool::new_without_request_logging(
        config.http_client_factory(),
        ClientRouteClass::Other,
    );
    let mut status = CheckStatus::Ok;
    match fetch_latest_github_release_version(&client).await {
        Ok(latest_version) => {
            details.push(format!("latest version: {latest_version}"));
            if is_newer(&latest_version, env!("CARGO_PKG_VERSION")) == Some(true) {
                details.push("latest version status: newer version is available".to_string());
            } else {
                details.push("latest version status: current version is not older".to_string());
            }
        }
        Err(err) => {
            status = CheckStatus::Warning;
            details.push(format!("latest version probe: {err}"));
        }
    }

    DoctorCheck::new(
        "updates.status",
        "updates",
        status,
        "Web2Codex release status checked",
    )
    .details(details)
}

fn update_action_label(context: &InstallContext) -> &'static str {
    match context.method {
        InstallMethod::Standalone { .. } => "web2codex update (GitHub Releases)",
        InstallMethod::Other => "manual or unknown installation",
        InstallMethod::Npm
        | InstallMethod::Bun
        | InstallMethod::VitePlus
        | InstallMethod::Pnpm
        | InstallMethod::Brew => "manual update (package-manager installs are unsupported)",
    }
}

async fn fetch_latest_github_release_version(
    client: &RouteAwareClientPool,
) -> Result<String, String> {
    #[derive(Deserialize)]
    struct ReleaseInfo {
        tag_name: String,
    }

    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        option_env!("CODEX_GITHUB_REPOSITORY").unwrap_or("angyedz/web2codex")
    );
    let info = http_get_json::<ReleaseInfo>(client, &url).await?;
    info.tag_name
        .strip_prefix("codex-web-v")
        .map(str::to_string)
        .ok_or_else(|| format!("failed to parse latest tag {}", info.tag_name))
}

async fn http_get_json<T>(client: &RouteAwareClientPool, url: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut response = client
            .request(Method::GET, url)
            .header(
                http::header::USER_AGENT,
                concat!("web2codex-doctor/", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|err| err.to_string())?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|err| err.to_string())? {
            if chunk.len() > MAX_VERSION_RESPONSE_BYTES.saturating_sub(body.len()) {
                return Err("version response exceeds size limit".to_string());
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|err| err.to_string())
    })
    .await
    .map_err(|_| "version request timed out".to_string())?
}

fn push_cached_version_details(details: &mut Vec<String>, version_file: &Path) {
    details.push(format!("version cache: {}", version_file.display()));
    match std::fs::read_to_string(version_file) {
        Ok(contents) => match serde_json::from_str::<VersionInfo>(&contents) {
            Ok(info) => {
                details.push(format!("cached latest version: {}", info.latest_version));
                if let Some(last_checked_at) = info.last_checked_at {
                    details.push(format!("last checked at: {last_checked_at}"));
                }
                if let Some(dismissed_version) = info.dismissed_version {
                    details.push(format!("dismissed version: {dismissed_version}"));
                }
            }
            Err(err) => details.push(format!("version cache parse: {err}")),
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            details.push("version cache: missing".to_string());
        }
        Err(err) => details.push(format!("version cache read: {err}")),
    }
}

fn is_newer(latest: &str, current: &str) -> Option<bool> {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => Some(latest > current),
        (Some(_), None) | (None, Some(_)) | (None, None) => None,
    }
}

fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.trim().split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

#[derive(Deserialize)]
struct VersionInfo {
    latest_version: String,
    #[serde(default)]
    last_checked_at: Option<String>,
    #[serde(default)]
    dismissed_version: Option<String>,
}
