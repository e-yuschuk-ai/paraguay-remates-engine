//! Operaciones de administración.

use axum::extract::{Path, State};
use axum::Json;
use remates_engine::{AuctionId, Outcome};
use remates_store::AdminBid;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AdminUser;
use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct CloseResponse {
    /// `false` si todavía no era la hora de cierre o ya estaba cerrada.
    pub closed: bool,
    pub outcome: Option<Outcome>,
    /// Número del certificado de adjudicación (N001…), si hubo venta.
    pub certificate_number: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/v1/admin/auctions/{auction_id}/bids",
    tag = "administración",
    params(("auction_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Historial con identidades reales, para trazabilidad (RF-13).", body = Vec<AdminBid>),
        (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    ),
    security(("bearer" = []))
)]
pub async fn admin_bids(
    State(st): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AdminBid>>, ApiError> {
    let id = AuctionId(id);
    st.store.standing(id).await?;
    Ok(Json(st.store.admin_history(id).await?))
}

#[utoipa::path(
    post,
    path = "/v1/admin/auctions/{auction_id}/close",
    tag = "administración",
    params(("auction_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Cierra ya si llegó la hora. El cierre también es automático.", body = CloseResponse),
        (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody),
        (status = 404, body = ErrorBody),
    ),
    security(("bearer" = []))
)]
pub async fn close_auction(
    State(st): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CloseResponse>, ApiError> {
    let closed = st.store.close_auction(AuctionId(id)).await?;
    tracing::info!(auction = %id, admin = %admin.id, closed = closed.is_some(), "cierre manual");
    Ok(Json(match closed {
        Some(c) => CloseResponse {
            closed: true,
            outcome: Some(c.outcome),
            certificate_number: c.certificate_number,
        },
        None => CloseResponse {
            closed: false,
            outcome: None,
            certificate_number: None,
        },
    }))
}
