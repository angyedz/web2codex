mod keys;
pub(crate) mod onboarding_screen;
mod provider_setup;
mod trust_directory;
pub(crate) use onboarding_screen::DirectoryTrustOptions;
pub(crate) use provider_setup::FieldDisplay;
pub(crate) use provider_setup::ProviderInput;
pub(crate) use provider_setup::move_provider_field;
pub(crate) use trust_directory::TrustCancelAction;
mod welcome;

pub(crate) const SETUP_CONNECTION_LOST: &str =
    "Codex lost its connection during setup. Restart Codex to continue.";
