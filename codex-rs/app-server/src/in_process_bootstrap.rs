//! Local network policy shared by embedded app-server transports.

use codex_core::config::Config;

/// Network policy established before an embedded runtime starts connecting.
#[derive(Clone, Default)]
pub struct EmbeddedNetworkPolicy {
    pub(crate) effective: codex_http_client::NetworkPolicyController,
    pub(crate) local: codex_http_client::NetworkPolicyController,
}

impl EmbeddedNetworkPolicy {
    /// Loads local application requirements; outbound traffic starts unavailable.
    pub async fn load(overrides: &codex_config::LoaderOverrides) -> Self {
        let policy = Self::default();
        let local = async {
            codex_config::loader::load_local_application_requirements(
                codex_exec_server::LOCAL_FS.as_ref(),
                overrides,
            )
            .await?
            .compose(Default::default())
        }
        .await;
        if let Ok(application) = local {
            policy.local.publish(
                policy.local.policy().revision(),
                crate::config_manager::application_network::destination_policy(
                    application.as_ref(),
                ),
            );
        }
        policy
    }

    /// Attaches the live application policy before an environment connects.
    pub fn bind(
        &self,
        factory: codex_http_client::HttpClientFactory,
    ) -> codex_http_client::HttpClientFactory {
        factory.with_network_policy(self.effective.policy())
    }

    /// Publishes loaded requirements before caller-owned clients are created.
    pub fn activate(&self, config: &mut Config) {
        let application = config
            .config_layer_stack
            .requirements()
            .application
            .as_ref()
            .map(|requirements| &requirements.value);
        self.effective.publish(
            self.effective.policy().revision(),
            crate::config_manager::application_network::destination_policy(application),
        );
        self.bind_config(config);
    }

    /// Keeps a rebuilt caller config on the active runtime policy.
    pub fn bind_config(&self, config: &mut Config) {
        config.application_network_policy = self.effective.policy();
    }
}
