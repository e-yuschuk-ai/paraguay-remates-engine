//! Errores HTTP con cuerpo JSON uniforme.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use remates_store::StoreError;
use serde::Serialize;

/// Cuerpo de todo error de la API.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    /// Código estable para el cliente, por ejemplo `not_found`.
    #[schema(example = "not_found")]
    pub error: &'static str,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("falta un token válido")]
    Unauthorized,
    #[error("no tenés permiso para esta operación")]
    Forbidden,
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Validation(String),
    #[error("error interno")]
    Internal,
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str) {
        match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            ApiError::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation"),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error) = self.parts();
        let body = ErrorBody {
            error,
            message: self.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::AuctionNotFound(_) => ApiError::NotFound("subasta no encontrada".into()),
            StoreError::IdempotencyConflict => ApiError::Conflict(e.to_string()),
            StoreError::InvalidRules(r) => ApiError::Validation(r.to_string()),
            StoreError::Db(ref db)
                if db
                    .as_database_error()
                    .is_some_and(|d| d.is_foreign_key_violation()) =>
            {
                ApiError::Validation("referencia a un usuario inexistente".into())
            }
            StoreError::Db(ref db)
                if db
                    .as_database_error()
                    .is_some_and(|d| d.is_unique_violation()) =>
            {
                ApiError::Conflict("el recurso ya existe".into())
            }
            other => {
                // No se filtran detalles internos al cliente.
                tracing::error!(error = %other, "error de almacenamiento");
                ApiError::Internal
            }
        }
    }
}
