//! Difusión en tiempo real (RF-15, RT-06).
//!
//! Cada instancia del backend lee **todo** el outbox en modo sólo lectura y reparte
//! cada evento a los WebSockets conectados a esa instancia, por subasta. No hace
//! falta un broker para correr varias instancias: todas leen la misma tabla.
//!
//! El outbox usa `bigserial`, cuyos `id` pueden confirmarse fuera de orden. El
//! lector avanza su cursor sólo por ids contiguos; si aparece un hueco espera hasta
//! [`GAP_TIMEOUT`] a que se complete (transacción en curso) y después lo da por
//! perdido (transacción revertida). Si aun así un cliente se pierde un evento, lo
//! detecta por el salto de `version` y pide un `resync`.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use remates_engine::AuctionId;
use remates_store::outbox::{self, OutboxEvent};
use remates_store::Db;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Cuánto se espera a que un `id` faltante del outbox se confirme.
pub const GAP_TIMEOUT: Duration = Duration::from_secs(3);
/// Consulta de respaldo si no llega ninguna notificación.
const POLL_EVERY: Duration = Duration::from_millis(500);
const BATCH: i64 = 500;

/// Reparto de eventos por subasta dentro de una instancia.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

struct Inner {
    channels: Mutex<HashMap<AuctionId, broadcast::Sender<Arc<OutboxEvent>>>>,
    capacity: usize,
}

impl Hub {
    /// `capacity`: eventos que puede atrasarse un cliente antes de pedirle `resync`.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                channels: Mutex::new(HashMap::new()),
                capacity,
            }),
        }
    }

    pub fn subscribe(&self, auction: AuctionId) -> broadcast::Receiver<Arc<OutboxEvent>> {
        let mut map = self.lock();
        map.entry(auction)
            .or_insert_with(|| broadcast::channel(self.inner.capacity).0)
            .subscribe()
    }

    /// Cantidad de subastas con al menos un suscriptor.
    pub fn active_channels(&self) -> usize {
        self.lock().len()
    }

    fn dispatch(&self, ev: OutboxEvent) {
        let id = AuctionId(ev.aggregate_id);
        let mut map = self.lock();
        if let Some(tx) = map.get(&id) {
            if tx.send(Arc::new(ev)).is_err() {
                // Nadie escucha esta subasta: se libera el canal.
                map.remove(&id);
            }
        }
    }

    fn sweep(&self) {
        self.lock().retain(|_, tx| tx.receiver_count() > 0);
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<AuctionId, broadcast::Sender<Arc<OutboxEvent>>>> {
        self.inner
            .channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Fija el punto de partida (el último evento existente) y lanza la tarea que
    /// reparte todo lo nuevo hasta que se cancele `shutdown`. Al volver, cualquier
    /// evento posterior ya va a llegar a los suscriptores.
    pub async fn start(
        &self,
        db: Db,
        shutdown: CancellationToken,
    ) -> Result<tokio::task::JoinHandle<()>, remates_store::StoreError> {
        let tail = Tail::new(outbox::last_id(&db).await?);
        let listener = outbox::listen(&db).await.ok();
        Ok(tokio::spawn(self.clone().run(db, tail, listener, shutdown)))
    }

    async fn run(
        self,
        db: Db,
        mut tail: Tail,
        mut listener: Option<sqlx::postgres::PgListener>,
        shutdown: CancellationToken,
    ) {
        let mut last_sweep = Instant::now();
        loop {
            match tail.poll(&db, |ev| self.dispatch(ev)).await {
                Ok(true) => continue, // lote lleno: seguir leyendo sin esperar
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "hub: error leyendo el outbox"),
            }
            if last_sweep.elapsed() > Duration::from_secs(60) {
                self.sweep();
                last_sweep = Instant::now();
            }
            let wait = if tail.has_gap() {
                Duration::from_millis(100)
            } else {
                POLL_EVERY
            };
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(wait) => {}
                n = async {
                    match listener.as_mut() {
                        Some(l) => l.recv().await.map(|_| ()),
                        None => std::future::pending().await,
                    }
                } => {
                    if n.is_err() {
                        // Se cortó la conexión de LISTEN: se reabre; mientras tanto, polling.
                        listener = outbox::listen(&db).await.ok();
                    }
                }
            }
        }
    }
}

/// Cursor tolerante a huecos sobre los `id` del outbox.
struct Tail {
    /// Todo `id <= cursor` ya se procesó o se dio por perdido.
    cursor: i64,
    /// Ids ya procesados por encima del cursor (hay un hueco antes de ellos).
    seen: BTreeSet<i64>,
    gap_since: Option<Instant>,
}

impl Tail {
    fn new(cursor: i64) -> Self {
        Self {
            cursor,
            seen: BTreeSet::new(),
            gap_since: None,
        }
    }

    fn has_gap(&self) -> bool {
        !self.seen.is_empty()
    }

    /// Procesa un lote. Devuelve `true` si el lote vino lleno.
    async fn poll(
        &mut self,
        db: &Db,
        mut emit: impl FnMut(OutboxEvent),
    ) -> Result<bool, remates_store::StoreError> {
        let skip: Vec<i64> = self.seen.iter().copied().collect();
        let batch = outbox::read_after(db, self.cursor, &skip, BATCH).await?;
        let full = batch.len() as i64 == BATCH;
        for ev in batch {
            self.seen.insert(ev.id);
            emit(ev);
        }
        self.advance(Instant::now());
        Ok(full)
    }

    fn advance(&mut self, now: Instant) {
        while self.seen.remove(&(self.cursor + 1)) {
            self.cursor += 1;
        }
        match (self.seen.first().copied(), self.gap_since) {
            (None, _) => self.gap_since = None,
            (Some(_), None) => self.gap_since = Some(now),
            (Some(first), Some(since)) if now.duration_since(since) >= GAP_TIMEOUT => {
                // Se da por perdido el hueco y se sigue desde el primer id visto.
                tracing::debug!(
                    from = self.cursor + 1,
                    to = first - 1,
                    "hub: hueco descartado"
                );
                self.cursor = first - 1;
                self.gap_since = None;
                self.advance(now);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_waits_for_gaps_then_gives_up() {
        let t0 = Instant::now();
        let mut tail = Tail::new(10);
        tail.seen.extend([11, 12, 14]);
        tail.advance(t0);
        assert_eq!(tail.cursor, 12);
        assert!(tail.has_gap(), "falta el 13");

        // Llega el 13 antes del timeout: avanza todo.
        tail.seen.insert(13);
        tail.advance(t0 + Duration::from_secs(1));
        assert_eq!(tail.cursor, 14);
        assert!(!tail.has_gap());

        // Hueco que nunca se completa (transacción revertida).
        tail.seen.insert(16);
        tail.advance(t0 + Duration::from_secs(2));
        assert_eq!(tail.cursor, 14);
        tail.advance(t0 + Duration::from_secs(2) + GAP_TIMEOUT);
        assert_eq!(tail.cursor, 16);
        assert!(!tail.has_gap());
    }
}
