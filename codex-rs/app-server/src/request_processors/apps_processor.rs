use super::*;

/// ChatGPT Apps are disabled in Web2Codex; MCP servers are the supported tool integration.
pub(crate) struct AppsRequestProcessor;

impl AppsRequestProcessor {
    pub(crate) fn new(
        _thread_manager: Arc<ThreadManager>,
        _outgoing: Arc<OutgoingMessageSender>,
        _config_manager: ConfigManager,
        _shutdown_token: CancellationToken,
    ) -> Self {
        Self
    }

    pub(crate) async fn apps_list(
        &self,
        _request_id: &ConnectionRequestId,
        _params: AppsListParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Ok(Some(
            AppsListResponse {
                data: Vec::new(),
                next_cursor: None,
            }
            .into(),
        ))
    }

    pub(crate) async fn apps_read(
        &self,
        params: AppsReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Ok(Some(
            AppsReadResponse {
                apps: Vec::new(),
                missing_app_ids: params.app_ids,
            }
            .into(),
        ))
    }

    pub(crate) async fn apps_installed(
        &self,
        _params: AppsInstalledParams,
    ) -> Result<AppsInstalledResponse, JSONRPCErrorError> {
        Ok(AppsInstalledResponse { apps: Vec::new() })
    }

    pub(crate) fn shutdown(&self) {}
}
