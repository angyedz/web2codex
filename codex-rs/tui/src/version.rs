/// The current Web2Codex CLI version as embedded at compile time.
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn runtime_version() -> &'static str {
    codex_build_info::runtime_version(CODEX_CLI_VERSION)
}
