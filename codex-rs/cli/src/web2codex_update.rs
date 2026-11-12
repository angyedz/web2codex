//! Update the installed Web2Codex package from this fork's GitHub Releases.

use anyhow::Context;
use anyhow::ensure;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::process::Command;
use std::time::Duration;

const RELEASE_INSTALL_SH: &str =
    "https://github.com/angyedz/web2codex/releases/latest/download/install.sh";
const RELEASE_INSTALL_PS1: &str =
    "https://github.com/angyedz/web2codex/releases/latest/download/install.ps1";
const MAX_INSTALLER_BYTES: u64 = 1024 * 1024;

pub(crate) fn run() -> anyhow::Result<()> {
    #[cfg(windows)]
    let (url, file_name) = (RELEASE_INSTALL_PS1, "install.ps1");
    #[cfg(not(windows))]
    let (url, file_name) = (RELEASE_INSTALL_SH, "install.sh");

    println!("Checking for the latest Web2Codex release...");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("web2codex/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("could not create the GitHub Releases client")?;
    let mut response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .with_context(|| format!("could not download the Web2Codex installer from {url}"))?;
    if let Some(length) = response.content_length() {
        ensure!(
            length <= MAX_INSTALLER_BYTES,
            "the Web2Codex installer is unexpectedly large ({length} bytes)"
        );
    }

    let mut installer = Vec::new();
    response
        .by_ref()
        .take(MAX_INSTALLER_BYTES + 1)
        .read_to_end(&mut installer)
        .context("could not read the Web2Codex installer")?;
    ensure!(
        !installer.is_empty() && installer.len() as u64 <= MAX_INSTALLER_BYTES,
        "the downloaded Web2Codex installer is empty or unexpectedly large"
    );

    let temp_dir = tempfile::Builder::new()
        .prefix("web2codex-update-")
        .tempdir()
        .context("could not create a temporary directory for the updater")?;
    let installer_path = temp_dir.path().join(file_name);
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
