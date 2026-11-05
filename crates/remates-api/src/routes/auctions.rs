//! Subastas y pujas.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{DateTime, TimeDelta, Utc};
use remates_engine::{
    Action, Amount, AuctionId, AuctionKind, Command, Currency, ExtensionMode, ExtensionPolicy,
    FirstBidRule, Rules, UserId,
};
use remates_store::{BidRequest, Decision, NewAuction, PublicBid, PublicStanding, Source};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{AuthUser, Role};
use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

/// Cabecera opcional para indicar que el pedido viene de la app móvil.
pub const CHANNEL_HEADER: &str = "x-client-channel";

#[derive(Debug, Deserialize, ToSchema)]
pub struct ExtensionBody {
    /// Ventana final en segundos: una puja dentro de ella alarga el cierre.
    #[schema(example = 120)]
    pub window_seconds: u32,
    /// Alargue en segundos. Cero lo desactiva.
    #[schema(example = 120)]
    pub extend_by_seconds: u32,
    #[serde(default)]
    pub mode: ExtensionMode,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateAuctionBody {
    /// Vendedor. Obligatorio si publica un administrador en su nombre (RF-05);
    /// si publica un vendedor, se toma del token.
    pub seller_id: Option<UserId>,
    pub currency: Currency,
    pub kind: AuctionKind,
    /// Escalón mínimo, en unidades menores.
    pub increment: Amount,
    pub opens_at: DateTime<Utc>,
    pub scheduled_close_at: DateTime<Utc>,
    /// Por defecto: 2 minutos de ventana y 2 de alargue (RF-12).
    pub extension: Option<ExtensionBody>,
    /// Primera oferta en remate al martillo. Pendiente P-04: por defecto, el incremento.
    #[serde(default)]
    pub first_bid: FirstBidRule,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedAuction {
    pub auction_id: AuctionId,
}

/// Puja manual. El postor es siempre el usuario del token.
#[derive(Debug, Deserialize, ToSchema)]
pub struct BidBody {
    /// Clave idempotente generada por el cliente para este intento. Reenviarla tras
    /// un corte devuelve la misma decisión sin duplicar la puja (RT-02).
    pub request_id: Uuid,
    /// Importe en unidades menores: centavos para USD, guaraníes para PYG.
    pub amount: Amount,
    /// Moneda que el usuario vio al confirmar (RF-10).
    pub currency: Currency,
}

/// Puja máxima automática (RF-11).
#[derive(Debug, Deserialize, ToSchema)]
pub struct MaxBidBody {
    pub request_id: Uuid,
    /// Máximo en unidades menores. Es privado: no aparece en eventos públicos.
    pub max: Amount,
    pub currency: Currency,
}

/// Decisión del motor. Una puja rechazada por reglas de la subasta responde 200 con
/// `status = rejected`: es un resultado de negocio, no un error del pedido.
#[derive(Debug, Serialize, ToSchema)]
pub struct DecisionResponse {
    pub decision: Decision,
    /// `true` si la clave ya se había usado y se devolvió la decisión guardada.
    pub replayed: bool,
}

#[utoipa::path(
    post,
    path = "/v1/auctions",
    tag = "subastas",
    request_body = CreateAuctionBody,
    responses(
        (status = 201, body = CreatedAuction),
        (status = 401, body = ErrorBody),
        (status = 403, body = ErrorBody),
        (status = 422, body = ErrorBody),
    ),
    security(("bearer" = []))
)]
pub async fn create_auction(
    State(st): State<AppState>,
    user: AuthUser,
    Json(body): Json<CreateAuctionBody>,
) -> Result<(StatusCode, Json<CreatedAuction>), ApiError> {
    let seller = if user.has(Role::Admin) {
        body.seller_id.ok_or_else(|| {
            ApiError::Validation("seller_id es obligatorio al publicar en nombre de otro".into())
        })?
    } else {
        user.require(Role::Seller)?;
        match body.seller_id {
            Some(s) if s != user.id => return Err(ApiError::Forbidden),
            _ => user.id,
        }
    };
    let extension = match body.extension {
        None => ExtensionPolicy::default(),
        Some(e) => ExtensionPolicy {
            window: TimeDelta::seconds(i64::from(e.window_seconds)),
            extend_by: TimeDelta::seconds(i64::from(e.extend_by_seconds)),
            mode: e.mode,
        },
    };
    let id = AuctionId::new();
    st.store
        .create_auction(NewAuction {
            id,
            seller,
            rules: Rules {
                currency: body.currency,
                kind: body.kind,
                increment: body.increment,
                opens_at: body.opens_at,
                scheduled_close_at: body.scheduled_close_at,
                extension,
                first_bid: body.first_bid,
            },
        })
        .await?;
    Ok((StatusCode::CREATED, Json(CreatedAuction { auction_id: id })))
}

#[utoipa::path(
    get,
    path = "/v1/auctions/{auction_id}",
    tag = "subastas",
    params(("auction_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Posición pública. También sirve de polling de respaldo (RF-15).", body = PublicStanding),
        (status = 404, body = ErrorBody),
    )
)]
pub async fn get_auction(
    State(st): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<PublicStanding>, ApiError> {
    Ok(Json(st.store.standing(AuctionId(id)).await?))
}

#[utoipa::path(
    get,
    path = "/v1/auctions/{auction_id}/bids",
    tag = "pujas",
    params(("auction_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Historial público, con alias (RF-13).", body = Vec<PublicBid>),
        (status = 404, body = ErrorBody),
    )
)]
pub async fn list_bids(
    State(st): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<PublicBid>>, ApiError> {
    let id = AuctionId(id);
    // Distinguir "sin pujas" de "no existe".
    st.store.standing(id).await?;
    Ok(Json(st.store.public_history(id).await?))
}

#[utoipa::path(
    post,
    path = "/v1/auctions/{auction_id}/bids",
    tag = "pujas",
    params(
        ("auction_id" = Uuid, Path),
        ("x-client-channel" = Option<String>, Header, description = "`app` si el pedido viene de la app móvil"),
    ),
    request_body = BidBody,
    responses(
        (status = 200, body = DecisionResponse),
        (status = 401, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, description = "La clave idempotente ya se usó con otro pedido.", body = ErrorBody),
    ),
    security(("bearer" = []))
)]
pub async fn place_bid(
    State(st): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<BidBody>,
) -> Result<Json<DecisionResponse>, ApiError> {
    submit(
        &st,
        &user,
        &headers,
        id,
        body.request_id,
        body.currency,
        Action::Bid {
            amount: body.amount,
        },
    )
    .await
}

#[utoipa::path(
    post,
    path = "/v1/auctions/{auction_id}/max-bids",
    tag = "pujas",
    params(
        ("auction_id" = Uuid, Path),
        ("x-client-channel" = Option<String>, Header, description = "`app` si el pedido viene de la app móvil"),
    ),
    request_body = MaxBidBody,
    responses(
        (status = 200, body = DecisionResponse),
        (status = 401, body = ErrorBody),
        (status = 404, body = ErrorBody),
        (status = 409, body = ErrorBody),
    ),
    security(("bearer" = []))
)]
pub async fn set_max_bid(
    State(st): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<MaxBidBody>,
) -> Result<Json<DecisionResponse>, ApiError> {
    submit(
        &st,
        &user,
        &headers,
        id,
        body.request_id,
        body.currency,
        Action::SetMax { max: body.max },
    )
    .await
}

async fn submit(
    st: &AppState,
    user: &AuthUser,
    headers: &HeaderMap,
    auction: Uuid,
    request_id: Uuid,
    currency: Currency,
    action: Action,
) -> Result<Json<DecisionResponse>, ApiError> {
    // Sólo `web` y `app` son canales públicos; WhatsApp y carga asistida entran por
    // conectores propios (P-01).
    let source = match headers.get(CHANNEL_HEADER).and_then(|v| v.to_str().ok()) {
        Some("app") => Source::App,
        _ => Source::Web,
    };
    let submitted = st
        .store
        .submit(BidRequest {
            request_id,
            auction_id: AuctionId(auction),
            command: Command {
                bidder: user.id,
                currency,
                action,
            },
            source,
        })
        .await?;
    Ok(Json(DecisionResponse {
        decision: submitted.decision,
        replayed: submitted.replayed,
    }))
}
