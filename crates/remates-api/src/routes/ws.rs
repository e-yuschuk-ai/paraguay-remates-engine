//! WebSocket de una subasta (RF-15, RF-17, RT-06).
//!
//! Protocolo (todo JSON, servidor → cliente):
//! 1. Al conectar: `{"type": "snapshot", "standing": …}`.
//! 2. Después: `{"type": "event", "version": n, "event_type": …, "payload": …}`.
//!
//! El cliente aplica los eventos con `version` mayor que la del último snapshot e
//! ignora los demás. Si detecta un salto de versión, o después de reconectar, envía
//! el texto `resync` y recibe un snapshot nuevo.
//!
//! El socket es de sólo lectura: las pujas van por `POST`, que es idempotente.
//! Con token (cabecera `Authorization` o `?access_token=`), además de los eventos
//! públicos llegan los avisos privados del usuario en esa subasta (oferta superada,
//! decisión de sus pujas, adjudicación).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::request::Parts;
use axum::response::Response;
use remates_engine::AuctionId;
use remates_store::outbox::OutboxEvent;
use remates_store::PublicStanding;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{self, AuthUser, Role};
use crate::error::{ApiError, ErrorBody};
use crate::state::AppState;

const PING_EVERY: Duration = Duration::from_secs(20);

#[derive(Debug, Deserialize)]
pub struct WsQuery {
    /// Token para navegadores, que no pueden mandar cabeceras en un WebSocket.
    pub access_token: Option<String>,
}

/// Mensaje del servidor por el WebSocket.
#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Snapshot {
        standing: PublicStanding,
    },
    Event {
        id: i64,
        version: i64,
        event_type: String,
        /// `public`, `user` o `admin`.
        audience: String,
        #[schema(value_type = Object)]
        payload: serde_json::Value,
    },
}

/// Usuario opcional del WebSocket: un token inválido es un error, la falta de token no.
pub struct OptionalUser(pub Option<AuthUser>);

impl axum::extract::FromRequestParts<AppState> for OptionalUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let query = Query::<WsQuery>::try_from_uri(&parts.uri)
            .map(|q| q.0.access_token)
            .unwrap_or(None);
        let token = auth::bearer(parts).map(str::to_owned).or(query);
        match token {
            None => Ok(OptionalUser(None)),
            Some(t) => state.auth.verify(&t).map(|u| OptionalUser(Some(u))),
        }
    }
}

#[utoipa::path(
    get,
    path = "/v1/auctions/{auction_id}/ws",
    tag = "tiempo real",
    params(
        ("auction_id" = Uuid, Path),
        ("access_token" = Option<String>, Query, description = "Token opcional para recibir avisos privados"),
    ),
    responses(
        (status = 101, description = "WebSocket. Cada mensaje es un `ServerMessage`.", body = ServerMessage),
        (status = 401, body = ErrorBody),
        (status = 404, body = ErrorBody),
    )
)]
pub async fn auction_ws(
    State(st): State<AppState>,
    Path(id): Path<Uuid>,
    OptionalUser(user): OptionalUser,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let id = AuctionId(id);
    // Suscribirse antes de leer el snapshot: ningún evento queda en el medio.
    let rx = st.hub.subscribe(id);
    let snapshot = st.store.standing(id).await?;
    Ok(ws.on_upgrade(move |socket| session(socket, st, id, user, rx, snapshot)))
}

fn visible(ev: &OutboxEvent, user: Option<&AuthUser>) -> bool {
    match ev.audience.as_str() {
        "public" => true,
        "user" => user.is_some_and(|u| ev.recipient_id == Some(u.id.0)),
        "admin" => user.is_some_and(|u| u.has(Role::Admin)),
        _ => false,
    }
}

async fn send(socket: &mut WebSocket, msg: &ServerMessage) -> bool {
    match serde_json::to_string(msg) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn resync(socket: &mut WebSocket, st: &AppState, id: AuctionId) -> bool {
    match st.store.standing(id).await {
        Ok(standing) => send(socket, &ServerMessage::Snapshot { standing }).await,
        Err(e) => {
            tracing::warn!(error = %e, auction = %id, "ws: no se pudo leer el snapshot");
            false
        }
    }
}

async fn session(
    mut socket: WebSocket,
    st: AppState,
    id: AuctionId,
    user: Option<AuthUser>,
    mut rx: tokio::sync::broadcast::Receiver<Arc<OutboxEvent>>,
    snapshot: PublicStanding,
) {
    if !send(&mut socket, &ServerMessage::Snapshot { standing: snapshot }).await {
        return;
    }
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) if visible(&ev, user.as_ref()) => {
                    let msg = ServerMessage::Event {
                        id: ev.id,
                        version: ev.aggregate_version,
                        event_type: ev.event_type.clone(),
                        audience: ev.audience.clone(),
                        payload: ev.payload.clone(),
                    };
                    if !send(&mut socket, &msg).await { break }
                }
                Ok(_) => {}
                // El cliente se atrasó: se le manda el estado completo.
                Err(RecvError::Lagged(_)) => if !resync(&mut socket, &st, id).await { break },
                Err(RecvError::Closed) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(t))) if t.as_str().trim() == "resync" => {
                    if !resync(&mut socket, &st, id).await { break }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break }
            }
        }
    }
}
