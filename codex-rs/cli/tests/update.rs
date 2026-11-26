use anyhow::Result;
use predicates::str::contains;
use std::path::PathBuf;
use tempfile::TempDir;

fn cli_binary() -> Result<PathBuf> {
    Ok(codex_utils_cargo_bin::cargo_bin("web2codex")
        .or_else(|_| codex_utils_cargo_bin::cargo_bin("codex"))?)
}

#[test]
fn cli_version_uses_installed_package_manifest() -> Result<()> {
    let package = TempDir::new()?;
    let bin_dir = package.path().join("bin");
    std::fs::create_dir(&bin_dir)?;
    let source = cli_binary()?;
    let executable = bin_dir.join(format!("web2codex{}", std::env::consts::EXE_SUFFIX));
    if std::fs::hard_link(&source, &executable).is_err() {
        std::fs::copy(&source, &executable)?;
    }
    let manifest = package.path().join("codex-package.json");
    std::fs::write(&manifest, r#"{"version":"9.8.7-prealpha.20261125"}"#)?;

    assert_cmd::Command::new(&executable)
        .env("WEB2CODEX_HOME", package.path().join("home"))
        .arg("--version")
        .assert()
        .success()
        .stdout(contains("9.8.7-prealpha.20261125"));

    std::fs::write(&manifest, r#"{"version":"invalid"}"#)?;
    assert_cmd::Command::new(&executable)
        .env("WEB2CODEX_HOME", package.path().join("home"))
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(env!("CARGO_PKG_VERSION")));

    Ok(())
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn update_does_not_start_interactive_prompt() -> Result<()> {
    let codex_home = TempDir::new()?;

    assert_cmd::Command::new(cli_binary()?)
        .env("WEB2CODEX_HOME", codex_home.path())
        .arg("update")
        .assert()
        .failure()
        .stderr(contains(
            "`web2codex update` is not available in debug builds",
        ));

    Ok(())
}
