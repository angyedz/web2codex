use std::sync::Arc;

use codex_analytics::AnalyticsEventsClient;
use codex_core::config::Config;
use codex_login::AuthManager;
use codex_runtime_agent_instance::RuntimeAuthSlot;

/// Product analytics uploads target ChatGPT's backend and are not part of this fork.
pub(crate) fn analytics_events_client_from_config(
    _auth_manager: Arc<AuthManager>,
    _config: &Config,
    _runtime_auth_slot: Option<RuntimeAuthSlot>,
) -> AnalyticsEventsClient {
    AnalyticsEventsClient::disabled()
}
