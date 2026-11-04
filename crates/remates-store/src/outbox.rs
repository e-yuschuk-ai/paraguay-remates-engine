//! Publicación de eventos del outbox (§5.1, RT-06).
//!
//! Los eventos se escriben en la misma transacción que el cambio que los origina,
//! así no se pierden avisos entre la confirmación de una puja y su envío. Un
//! publicador los toma en lotes y los entrega (tiempo real, notificaciones,
//! conectores de WhatsApp).
//!
//! Garantía: entrega *al menos una vez*. Con varios publicadores en paralelo el
//! orden global no está garantizado; los consumidores ordenan por subasta con
//! `aggregate_version` y descartan duplicados por `id`.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::db::Db;
use crate::error::StoreError;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct OutboxEvent {
    pub id: i64,
    pub aggregate_id: Uuid,
    pub aggregate_version: i64,
    pub event_type: String,
    /// `public`, `user` o `admin`.
    pub audience: String,
    pub recipient_id: Option<Uuid>,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Error al publicar un lote.
#[derive(Debug, thiserror::Error)]
pub enum RelayError<E> {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// El publicador falló: el lote queda pendiente y se reintenta.
    #[error("falló la publicación: {0}")]
    Publish(E),
}

/// Toma hasta `limit` eventos pendientes, los pasa a `publish` y, si tuvo éxito,
/// los marca como publicados. Devuelve cuántos publicó.
///
/// Los eventos tomados quedan bloqueados (`FOR UPDATE SKIP LOCKED`) mientras dura
/// la publicación, así varios publicadores no toman el mismo lote.
pub async fn relay_batch<E>(
    db: &Db,
    limit: i64,
    publish: impl AsyncFnOnce(&[OutboxEvent]) -> Result<(), E>,
) -> Result<usize, RelayError<E>> {
    let mut tx = db.primary().begin().await.map_err(StoreError::from)?;
    let events: Vec<OutboxEvent> = sqlx::query_as(
        "SELECT id, aggregate_id, aggregate_version, event_type, audience, recipient_id, \
         payload, created_at FROM outbox WHERE published_at IS NULL \
         ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED",
    )
    .bind(limit)
    .fetch_all(&mut *tx)
    .await
    .map_err(StoreError::from)?;
    if events.is_empty() {
        return Ok(0);
    }
    publish(&events).await.map_err(RelayError::Publish)?;
    let ids: Vec<i64> = events.iter().map(|e| e.id).collect();
    sqlx::query("UPDATE outbox SET published_at = clock_timestamp() WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&mut *tx)
        .await
        .map_err(StoreError::from)?;
    tx.commit().await.map_err(StoreError::from)?;
    Ok(events.len())
}
