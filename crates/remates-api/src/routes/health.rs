//! Sondas para el orquestador.

use axum::extract::State;
use axum::http::StatusCode;

use crate::state::AppState;

#[utoipa::path(get, path = "/health", tag = "operación",
    responses((status = 200, description = "El proceso está vivo.")))]
pub async fn health() -> &'static str {
    "ok"
}

#[utoipa::path(get, path = "/ready", tag = "operación",
    responses(
        (status = 200, description = "Puede atender: la base primaria responde."),
        (status = 503, description = "La base no responde."),
    ))]
pub async fn ready(State(st): State<AppState>) -> StatusCode {
    match sqlx::query("SELECT 1")
        .execute(st.store.db().primary())
        .await
    {
        Ok(_) => StatusCode::OK,
        Err(e) => {
            tracing::warn!(error = %e, "ready: la base no responde");
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
