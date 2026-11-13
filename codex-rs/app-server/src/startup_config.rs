//! One-shot configuration discovery. Never initializes environments, thread storage, or plugins.

use crate::config_manager::ConfigManager;
use crate::request_processors::config_errors::config_load_error;
use crate::request_processors::config_processor::map_requirements_to_api;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ConfigRequirementsReadResponse;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_arg0::Arg0DispatchPaths;
use codex_config::LoaderOverrides;
use codex_config::NoopThreadConfigLoader;
use codex_core::config::find_codex_home;
use codex_login::default_client::SetOriginatorError;
use codex_login::default_client::USER_AGENT_SUFFIX;
use codex_login::default_client::set_default_originator;
use codex_utils_cli::CliConfigOverrides;
use serde::Serialize;
use std::io;
use std::io::Write;
use std::sync::Arc;

#[derive(Debug, clap::Args)]
pub struct StartupConfigArgs {
    /// Validate configuration and print requirements as JSON without starting the app server.
    #[arg(long, conflicts_with_all = ["listen", "stdio", "remote_control", "managed_daemon"])]
    pub print_startup_config: bool,

    /// ClientInfo JSON used for configuration/auth request identity.
    #[arg(long, requires = "print_startup_config", value_parser = |value: &str| serde_json::from_str::<ClientInfo>(value))]
    pub startup_client_info: Option<ClientInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartupConfig {
    app_server_version: &'static str,
    config_requirements: ConfigRequirementsReadResponse,
}

/// Writes exactly one JSON value, including structured errors on failure.
pub async fn print_startup_config(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
    startup: StartupConfigArgs,
) -> io::Result<()> {
    let result = read_startup_config(
        arg0_paths,
        cli_config_overrides,
        loader_overrides,
        strict_config,
        startup,
    )
    .await;
    let output = match &result {
        Ok(config) => serde_json::to_vec(config)?,
        Err(error) => serde_json::to_vec(&serde_json::json!({ "error": error }))?,
    };
    let mut stdout = io::stdout().lock();
    stdout.write_all(&output)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    result
        .map(|_| ())
        .map_err(|_| io::Error::other("startup configuration unavailable"))
}

async fn read_startup_config(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
    loader_overrides: LoaderOverrides,
    strict_config: bool,
    startup: StartupConfigArgs,
) -> Result<StartupConfig, JSONRPCErrorError> {
    if let Some(client) = startup.startup_client_info {
        match set_default_originator(client.name.clone()) {
            Ok(()) | Err(SetOriginatorError::AlreadyInitialized) => {}
            Err(SetOriginatorError::InvalidHeaderValue) => {
                return Err(crate::error_code::invalid_params("invalid client name"));
            }
        }
        if let Ok(mut suffix) = USER_AGENT_SUFFIX.lock() {
            *suffix = Some(format!("{}; {}", client.name, client.version));
        }
    }
    let loader_overrides = crate::loader_overrides_with_test_user_config_file(
        loader_overrides,
        crate::test_user_config_file_from_env(),
    )
    .map_err(startup_error)?;
    let overrides = cli_config_overrides
        .parse_overrides()
        .map_err(|error| startup_error(io::Error::new(io::ErrorKind::InvalidInput, error)))?;
    let manager = ConfigManager::new(
        find_codex_home().map_err(startup_error)?.to_path_buf(),
        overrides,
        loader_overrides,
        strict_config,
        Default::default(),
        arg0_paths,
        Arc::new(NoopThreadConfigLoader),
    );
    let cwd = std::env::current_dir().ok();
    let config = manager
        .load_startup_config(cwd.clone())
        .await
        .map_err(startup_error)?
        .config;
    // Validate and emit the fetched snapshot, or the fresh signed-out snapshot.
    config
        .validate_windows_mxc_requirement()
        .map_err(startup_error)?;
    let auth_config = config.auth_config();
    auth_config.validate().map_err(startup_error)?;
    let allowed_login_methods = auth_config.managed_auth_policy.allowed_login_methods(
        auth_config.forced_login_method,
        auth_config.forced_chatgpt_workspace_id.as_deref(),
    );
    let requirements = config.config_layer_stack.requirements_toml();
    Ok(StartupConfig {
        app_server_version: env!("CARGO_PKG_VERSION"),
        config_requirements: ConfigRequirementsReadResponse {
            supports_independent_speed_modes: Some(true),
            requirements: map_requirements_to_api(
                (!requirements.is_empty()).then(|| requirements.clone()),
                allowed_login_methods,
            ),
        },
    })
}

fn startup_error(error: io::Error) -> JSONRPCErrorError {
    let mut response = config_load_error(&error);
    if response.data.is_none() {
        let warning = crate::config_warning_from_error("Invalid configuration", &error);
        response.data = Some(serde_json::json!({
            "reason": "configLoad",
            "filePath": warning.path,
            "line": warning.range.as_ref().map(|range| range.start.line),
            "column": warning.range.as_ref().map(|range| range.start.column),
            "detail": error.to_string(),
        }));
    }
    response
}
