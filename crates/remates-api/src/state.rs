use std::sync::Arc;

use remates_store::Store;

use crate::auth::JwtAuth;
use crate::hub::Hub;

/// Estado compartido por todos los handlers. Barato de clonar.
#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub auth: Arc<JwtAuth>,
    pub hub: Hub,
}
