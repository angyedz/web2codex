use super::*;

mod enterprise_login;

pub(super) use enterprise_login::EnterpriseLoginCompletion;
pub(super) use enterprise_login::EnterpriseLoginTarget;

#[derive(Clone)]
pub(crate) struct AccountRequestProcessor {
    pub(super) enterprise_login: Arc<enterprise_login::EnterpriseLoginState>,
}

impl AccountRequestProcessor {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        thread_manager: Arc<ThreadManager>,
        config_manager: ConfigManager,
    ) -> Arc<Self> {
        Arc::new(Self {
            enterprise_login: Arc::new(enterprise_login::EnterpriseLoginState::new(
                auth_manager,
                thread_manager,
                config_manager,
            )),
        })
    }

    pub(crate) async fn cancel_active_login(&self) {
        self.enterprise_login.cancel(/*login_id*/ None).await;
    }
}
