use remates_engine::{AuctionId, RulesError};

/// Errores de infraestructura o de uso. Los rechazos de negocio no son errores:
/// viajan como [`crate::Decision::Rejected`] y quedan registrados.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("subasta {0} no encontrada")]
    AuctionNotFound(AuctionId),
    #[error("la clave de idempotencia ya se usó con otro pedido")]
    IdempotencyConflict,
    #[error(transparent)]
    InvalidRules(#[from] RulesError),
    #[error("datos inconsistentes en la base: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl StoreError {
    pub(crate) fn corrupt(msg: impl Into<String>) -> Self {
        StoreError::Corrupt(msg.into())
    }
}
