//! Validates sandbox provisioning against local configuration; registration-only refresh
//! retains the existing sandbox and is admitted by the registered-runtime transaction.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use codex_config::ConfigLoadOptions;
use codex_config::ConfigRequirements;
use codex_config::types::WindowsSandboxModeToml;
use codex_core::config::load_config_toml_with_layer_stack;
use codex_windows_sandbox::WindowsSandboxProvisioningSettings;
use codex_windows_sandbox::WindowsSandboxProxyListeners;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Notify;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::ImpersonateLoggedOnUser;

use crate::ipc::ProvisioningRequest;

struct NonOwningImpersonationToken(HANDLE);

// SAFETY: Windows access token handles are opaque process-wide values that may
// be shared between threads. This wrapper does not own the token or extend its
// lifetime.
unsafe impl Send for NonOwningImpersonationToken {}
unsafe impl Sync for NonOwningImpersonationToken {}

pub(crate) fn validate_provisioning_request(
    codex_home: &Path,
    request: &ProvisioningRequest,
    impersonation_token: HANDLE,
) -> Result<()> {
    if request.registered_core && request.refresh_only {
        // The caller is already authenticated. registered::run checks the existing
        // owner, accounts and unchanged settings under the setup lock; it cannot
        // provision or repair a sandbox. Existing local requirements remain in force.
        return Ok(());
    }
    let impersonation_failure = Arc::new(Notify::new());
    let worker_impersonation_failure = Arc::clone(&impersonation_failure);
    let impersonation_token = NonOwningImpersonationToken(impersonation_token);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .on_thread_start(move || {
            // Capture the wrapper rather than its raw pointer field.
            let impersonation_token = &impersonation_token;
            if unsafe { ImpersonateLoggedOnUser(impersonation_token.0) } == 0 {
                let error = std::io::Error::last_os_error();
                worker_impersonation_failure.notify_one();
                panic!(
                    "failed to impersonate provisioning client on configuration runtime thread: {error}"
                );
            }
        })
        .build()
        .context("start configuration runtime")?;
    let requirements = runtime.block_on(async {
        tokio::select! {
            biased;
            () = impersonation_failure.notified() => {
                Err(anyhow::anyhow!("configuration runtime worker failed to impersonate the provisioning client"))
            },
            result = async {
                let mut bootstrap_config = load_config_toml_with_layer_stack(
                    codex_home,
                    /*cwd*/ None,
                    Vec::new(),
                    ConfigLoadOptions::default(),
                )
                .await
                .context("load bootstrap configuration")?;
                Ok::<_, anyhow::Error>(
                    bootstrap_config.config_layer_stack.requirements().clone(),
                )
            } => result,
        }
    })?;

    validate_requirements(&request.settings, &request.listeners, &requirements)
        .context("enforce configured provisioning requirements")
}

fn validate_requirements(
    settings: &WindowsSandboxProvisioningSettings,
    listeners: &WindowsSandboxProxyListeners,
    requirements: &ConfigRequirements,
) -> Result<()> {
    if requirements
        .windows_sandbox_mode
        .can_set(&Some(WindowsSandboxModeToml::Elevated))
        .is_err()
    {
        bail!("configured requirements do not permit the elevated Windows sandbox");
    }

    let Some(network) = requirements.network.as_ref().map(|network| &network.value) else {
        return Ok(());
    };
    if network.enabled == Some(false)
        && (settings.allow_local_binding || !settings.proxy_ports.is_empty())
    {
        bail!("configured requirements disable sandbox network access");
    }
    if settings.allow_local_binding && network.allow_local_binding == Some(false) {
        bail!("configured requirements do not permit local network binding");
    }
    if let Some(required) = network.http_port
        && let Some(actual) = listeners.http_ports.iter().find(|port| **port != required)
    {
        bail!("configured requirements do not permit HTTP proxy port {actual}");
    }
    if let Some(required) = network.socks_port
        && let Some(actual) = listeners.socks_ports.iter().find(|port| **port != required)
    {
        bail!("managed policy does not permit SOCKS proxy port {actual}");
    }
    // Omitting listener identities must not bypass managed port restrictions.
    if network.http_port.is_some() || network.socks_port.is_some() {
        for port in &settings.proxy_ports {
            if !listeners.http_ports.contains(port)
                && !listeners.socks_ports.contains(port)
                && network.http_port != Some(*port)
                && network.socks_port != Some(*port)
            {
                bail!("managed policy does not permit unclassified proxy port {port}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "machine_policy_tests.rs"]
mod tests;
