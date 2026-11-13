//! Update the installed Web2Codex package from this fork's GitHub Releases.

use anyhow::Context;
use anyhow::ensure;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::process::Command;
use std::time::Duration;
use url::Url;

const RELEASE_METADATA_URL: &str = "https://api.github.com/repos/angyedz/web2codex/releases/latest";
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/angyedz/web2codex/releases/download/";
#[cfg(not(windows))]
const INSTALLER_NAME: &str = "install.sh";
#[cfg(not(windows))]
const INSTALLER_FILE_NAME: &str = "install.sh";
#[cfg(windows)]
const INSTALLER_NAME: &str = "install.ps1";
#[cfg(windows)]
const INSTALLER_FILE_NAME: &str = "install.ps1";
const MAX_RELEASE_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INSTALLER_BYTES: u64 = 1024 * 1024;

#[derive(Deserialize)]
struct GithubRelease {
    assets: Vec<GithubReleaseAsset>,
}

#[derive(Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

pub(crate) fn run() -> anyhow::Result<()> {
    println!("Checking for the latest Web2Codex release...");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("web2codex/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("could not create the GitHub Releases client")?;

    let metadata_response = client
        .get(RELEASE_METADATA_URL)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .context("could not fetch the latest Web2Codex release metadata")?;
    let metadata_bytes = read_response_limited(
        metadata_response,
        MAX_RELEASE_METADATA_BYTES,
        "GitHub release metadata",
    )?;
    let release: GithubRelease = serde_json::from_slice(&metadata_bytes)
        .context("GitHub returned invalid Web2Codex release metadata")?;
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == INSTALLER_NAME)
        .with_context(|| format!("latest Web2Codex release has no {INSTALLER_NAME} asset"))?;
    let expected_sha256 = asset
        .digest
        .as_deref()
        .context("GitHub did not provide a SHA-256 digest for the Web2Codex installer")?
        .strip_prefix("sha256:")
        .context("GitHub provided an unsupported Web2Codex installer digest format")?;
    ensure!(
        expected_sha256.len() == 64 && expected_sha256.as_bytes().iter().all(u8::is_ascii_hexdigit),
        "GitHub provided an invalid SHA-256 digest for the Web2Codex installer"
    );

    let download_url = Url::parse(&asset.browser_download_url)
        .context("GitHub provided an invalid Web2Codex installer URL")?;
    ensure!(
        download_url.scheme() == "https"
            && download_url.host_str() == Some("github.com")
            && asset
                .browser_download_url
                .starts_with(RELEASE_DOWNLOAD_PREFIX)
            && download_url.path().ends_with(&format!("/{INSTALLER_NAME}")),
        "GitHub provided an unexpected Web2Codex installer URL"
    );

    let installer_response = client
        .get(download_url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .with_context(|| {
            format!(
                "could not download the Web2Codex installer from {}",
                asset.browser_download_url
            )
        })?;
    let installer = read_response_limited(
        installer_response,
        MAX_INSTALLER_BYTES,
        "Web2Codex installer",
    )?;
    ensure!(
        !installer.is_empty(),
        "the downloaded Web2Codex installer is empty"
    );
    let actual_sha256 = format!("{:x}", Sha256::digest(&installer));
    ensure!(
        actual_sha256.eq_ignore_ascii_case(expected_sha256),
        "the downloaded Web2Codex installer failed SHA-256 verification"
    );

    let temp_dir = tempfile::Builder::new()
        .prefix("web2codex-update-")
        .tempdir()
        .context("could not create a temporary directory for the updater")?;
    let installer_path = temp_dir.path().join(INSTALLER_FILE_NAME);
    let mut installer_file = File::create(&installer_path)
        .context("could not create the temporary Web2Codex installer")?;
    installer_file
        .write_all(&installer)
        .context("could not save the temporary Web2Codex installer")?;
    installer_file
        .sync_all()
        .context("could not flush the temporary Web2Codex installer")?;
    drop(installer_file);

    println!("Installing the latest Web2Codex release...");
    #[cfg(windows)]
    let status = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&installer_path)
        .env("CODEX_NON_INTERACTIVE", "1")
        .env("CODEX_GITHUB_REPOSITORY", "angyedz/web2codex")
        .env("CODEX_GITHUB_TAG_PREFIX", "codex-web-v")
        .env("CODEX_COMMAND_NAME", "web2codex")
        .status()
        .context("could not start the Web2Codex PowerShell installer")?;
    #[cfg(not(windows))]
    let status = Command::new("sh")
        .arg(&installer_path)
        .env("CODEX_NON_INTERACTIVE", "1")
        .env("CODEX_GITHUB_REPOSITORY", "angyedz/web2codex")
        .env("CODEX_GITHUB_TAG_PREFIX", "codex-web-v")
        .env("CODEX_COMMAND_NAME", "web2codex")
        .status()
        .context("could not start the Web2Codex shell installer")?;
    ensure!(
        status.success(),
        "the Web2Codex installer failed with {status}"
    );

    println!("Web2Codex is up to date. Restart the CLI to use the new version.");
    Ok(())
}

fn read_response_limited(
    mut response: reqwest::blocking::Response,
    max_bytes: u64,
    description: &str,
) -> anyhow::Result<Vec<u8>> {
    if let Some(length) = response.content_length() {
        ensure!(
            length <= max_bytes,
            "the {description} is unexpectedly large ({length} bytes)"
        );
    }

    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read the {description}"))?;
    ensure!(
        bytes.len() as u64 <= max_bytes,
        "the {description} is unexpectedly large"
    );
    Ok(bytes)
}
