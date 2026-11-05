//! Operaciones del motor sobre PostgreSQL.
//!
//! Cada cambio de una subasta ocurre en **una** transacción que:
//! 1. bloquea la fila de la subasta (`SELECT … FOR UPDATE`), lo que serializa
//!    pujas y cierre de esa subasta sin frenar a las demás (RT-01, RT-03);
//! 2. lee el reloj de la base;
//! 3. aplica el motor;
//! 4. escribe estado, historial, decisión y eventos de outbox juntos (§5.1).
//!
//! No hay estado en memoria: cualquier cantidad de instancias del backend puede
//! atender pedidos en paralelo.

use chrono::{DateTime, Utc};
use remates_engine::{
    Action, Amount, Applied, Auction, AuctionId, CloseRejection, Command, Currency, Outcome, Phase,
    RecordKind, Rejection, UserId,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use crate::clock::Clock;
use crate::db::Db;
use crate::error::StoreError;
use crate::rows::{self, db_i64};

/// Canal por el que llegó un pedido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Web,
    App,
    Whatsapp,
    /// Carga asistida por un administrador (por ejemplo, una puja recibida por WhatsApp).
    Admin,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Web => "web",
            Source::App => "app",
            Source::Whatsapp => "whatsapp",
            Source::Admin => "admin",
        }
    }
}

/// Subasta a crear.
#[derive(Debug, Clone)]
pub struct NewAuction {
    pub id: AuctionId,
    pub seller: UserId,
    pub rules: remates_engine::Rules,
}

/// Pedido de puja con su clave idempotente (RT-02, §5.4).
#[derive(Debug, Clone, Copy)]
pub struct BidRequest {
    /// Clave generada por el cliente para este intento. Repetirla tras un corte
    /// devuelve la decisión ya tomada.
    pub request_id: Uuid,
    pub auction_id: AuctionId,
    pub command: Command,
    pub source: Source,
}

/// Decisión sobre un pedido. Es lo que se devuelve al cliente y lo que se guarda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Decision {
    Accepted(Accepted),
    Rejected { reason: RejectReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Accepted {
    /// Orden del servidor de esta acción.
    pub seq: u64,
    pub is_leader: bool,
    pub your_alias: u32,
    pub leader_alias: u32,
    pub price: Amount,
    pub next_min: Amount,
    pub closes_at: DateTime<Utc>,
    pub extended: bool,
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RejectReason {
    /// Usuario inexistente, suspendido o sin identidad/2FA vigente (RF-03, RF-10).
    BidderNotEligible,
    Engine {
        rejection: Rejection,
    },
}

/// Resultado de [`Store::submit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submitted {
    pub decision: Decision,
    /// `true` si la clave ya existía y se devolvió la decisión guardada.
    pub replayed: bool,
}

/// Subasta recién cerrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed {
    pub auction_id: AuctionId,
    pub outcome: Outcome,
    /// Número del certificado de adjudicación, si hubo venta.
    pub certificate_number: Option<i64>,
    pub version: u64,
}

/// Posición pública (sin identidades ni máximos).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicStanding {
    pub auction_id: AuctionId,
    pub currency: Currency,
    pub phase: Phase,
    pub leader_alias: Option<u32>,
    pub price: Option<Amount>,
    pub next_min: Option<Amount>,
    pub closes_at: DateTime<Utc>,
    pub version: u64,
    pub server_time: DateTime<Utc>,
}

/// Línea del historial público.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PublicBid {
    pub seq: u64,
    pub alias: u32,
    pub amount: Amount,
    pub kind: RecordKind,
    pub created_at: DateTime<Utc>,
}

/// Línea del historial con identidad real, para trazabilidad autorizada (RF-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AdminBid {
    pub seq: u64,
    pub bidder: UserId,
    pub alias: u32,
    pub amount: Amount,
    pub kind: RecordKind,
    pub source: Source,
    pub request_id: Uuid,
    pub created_at: DateTime<Utc>,
}

/// Punto de entrada del motor persistente.
#[derive(Debug, Clone)]
pub struct Store {
    db: Db,
    clock: Clock,
}

impl Store {
    pub fn new(db: Db, clock: Clock) -> Self {
        Self { db, clock }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Registra una subasta nueva, sin pujas.
    pub async fn create_auction(&self, new: NewAuction) -> Result<(), StoreError> {
        let auction = Auction::new(new.id, new.seller, new.rules)?;
        let r = auction.rules();
        let (kind, base) = rows::kind_str(&r.kind);
        sqlx::query(
            "INSERT INTO auctions (id, seller_id, currency, kind, base_price, increment, \
             first_bid_rule, opens_at, scheduled_close_at, closes_at, extension_window_ms, \
             extension_ms, extension_mode) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10, $11, $12)",
        )
        .bind(new.id.0)
        .bind(new.seller.0)
        .bind(r.currency.code())
        .bind(kind)
        .bind(base)
        .bind(r.increment.minor())
        .bind(rows::first_bid_str(r.first_bid))
        .bind(r.opens_at)
        .bind(r.scheduled_close_at)
        .bind(r.extension.window.num_milliseconds())
        .bind(r.extension.extend_by.num_milliseconds())
        .bind(rows::mode_str(r.extension.mode))
        .execute(self.db.primary())
        .await?;
        Ok(())
    }

    /// Procesa un pedido de puja o de máximo automático.
    ///
    /// Devuelve `Ok` con la decisión (aceptada o rechazada) siempre que la base
    /// responda; la decisión queda registrada y genera un evento `bid.decision`
    /// para el canal de origen (por ejemplo, la confirmación por WhatsApp, RF-22).
    pub async fn submit(&self, req: BidRequest) -> Result<Submitted, StoreError> {
        let mut tx = self.db.primary().begin().await?;
        let mut auction = self.lock_auction(&mut tx, req.auction_id).await?;
        let now = self.clock.now(&mut tx).await?;

        // La clave se mira con el bloqueo tomado: dos reintentos simultáneos de la
        // misma puja quedan serializados y el segundo ve la decisión del primero.
        if let Some(previous) = find_request(&mut tx, &req).await? {
            tx.rollback().await?;
            return Ok(Submitted {
                decision: previous,
                replayed: true,
            });
        }

        let decision = if !bidder_can_bid(&mut tx, req.command.bidder).await? {
            Decision::Rejected {
                reason: RejectReason::BidderNotEligible,
            }
        } else {
            match auction.apply(req.command, now) {
                Ok(applied) => {
                    persist_applied(&mut tx, &auction, &applied, &req, now).await?;
                    let alias = |u: UserId| auction.bidder(u).map(|b| b.alias).unwrap_or(0);
                    Decision::Accepted(Accepted {
                        seq: applied.seq,
                        is_leader: applied.leader == req.command.bidder,
                        your_alias: alias(req.command.bidder),
                        leader_alias: alias(applied.leader),
                        price: applied.price,
                        next_min: applied.next_min,
                        closes_at: applied.closes_at,
                        extended: applied.extended,
                        version: applied.version,
                    })
                }
                Err(rejection) => Decision::Rejected {
                    reason: RejectReason::Engine { rejection },
                },
            }
        };

        record_request(&mut tx, &req, &decision, now).await?;
        insert_event(
            &mut tx,
            req.auction_id,
            auction.state().version,
            "bid.decision",
            Audience::User(req.command.bidder),
            json!({
                "auction_id": req.auction_id,
                "request_id": req.request_id,
                "source": req.source,
                "decision": decision,
                "server_time": now,
            }),
        )
        .await?;
        match tx.commit().await {
            Ok(()) => {}
            Err(e) if is_request_conflict(&e) => return Err(StoreError::IdempotencyConflict),
            Err(e) => return Err(e.into()),
        }
        Ok(Submitted {
            decision,
            replayed: false,
        })
    }

    /// Cierra una subasta si ya llegó su hora. `None` si todavía no o si ya estaba cerrada.
    pub async fn close_auction(&self, id: AuctionId) -> Result<Option<Closed>, StoreError> {
        let mut tx = self.db.primary().begin().await?;
        let auction = self.lock_auction(&mut tx, id).await?;
        let now = self.clock.now(&mut tx).await?;
        self.close_locked(tx, auction, now).await
    }

    /// Cierra hasta `limit` subastas vencidas. Varios procesos pueden llamarla en
    /// paralelo: `SKIP LOCKED` reparte las subastas sin que dos tomen la misma, y
    /// una subasta con una puja en curso se saltea y se cierra en la próxima vuelta.
    pub async fn close_due(&self, limit: usize) -> Result<Vec<Closed>, StoreError> {
        let mut closed = Vec::new();
        for _ in 0..limit {
            let mut tx = self.db.primary().begin().await?;
            let now = self.clock.now(&mut tx).await?;
            let id: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM auctions WHERE status = 'active' AND closes_at <= $1 \
                 ORDER BY closes_at LIMIT 1 FOR UPDATE SKIP LOCKED",
            )
            .bind(now)
            .fetch_optional(&mut *tx)
            .await?;
            let Some(id) = id else { break };
            let auction = self.lock_auction(&mut tx, AuctionId(id)).await?;
            if let Some(c) = self.close_locked(tx, auction, now).await? {
                closed.push(c);
            }
        }
        Ok(closed)
    }

    /// Posición pública, leída de la réplica.
    pub async fn standing(&self, id: AuctionId) -> Result<PublicStanding, StoreError> {
        let mut conn = self.db.replica().acquire().await?;
        let state = rows::load_auction(&mut conn, id, false)
            .await?
            .ok_or(StoreError::AuctionNotFound(id))?;
        let auction = Auction::from_state(state).map_err(|e| StoreError::corrupt(e.0))?;
        let now = self.clock.now(&mut conn).await?;
        let s = auction.standing(now);
        Ok(PublicStanding {
            auction_id: id,
            currency: auction.rules().currency,
            phase: s.phase,
            leader_alias: s.leader_alias,
            price: s.price,
            next_min: s.next_min,
            closes_at: s.closes_at,
            version: s.version,
            server_time: now,
        })
    }

    /// Historial público, leído de la réplica.
    pub async fn public_history(&self, id: AuctionId) -> Result<Vec<PublicBid>, StoreError> {
        Ok(self
            .admin_history(id)
            .await?
            .into_iter()
            .map(|b| PublicBid {
                seq: b.seq,
                alias: b.alias,
                amount: b.amount,
                kind: b.kind,
                created_at: b.created_at,
            })
            .collect())
    }

    /// Historial con identidades reales. La API debe restringirlo a administradores.
    pub async fn admin_history(&self, id: AuctionId) -> Result<Vec<AdminBid>, StoreError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            seq: i64,
            bidder_id: Uuid,
            alias: i32,
            amount: i64,
            kind: String,
            source: String,
            request_id: Uuid,
            created_at: DateTime<Utc>,
        }
        let rows_: Vec<Row> = sqlx::query_as(
            "SELECT b.seq, b.bidder_id, ab.alias, b.amount, b.kind, b.source, b.request_id, b.created_at \
             FROM bids b JOIN auction_bidders ab \
               ON ab.auction_id = b.auction_id AND ab.bidder_id = b.bidder_id \
             WHERE b.auction_id = $1 ORDER BY b.seq",
        )
        .bind(id.0)
        .fetch_all(self.db.replica())
        .await?;
        rows_
            .into_iter()
            .map(|r| {
                Ok(AdminBid {
                    seq: rows::seq(r.seq)?,
                    bidder: UserId(r.bidder_id),
                    alias: u32::try_from(r.alias).map_err(|_| StoreError::corrupt("alias"))?,
                    amount: rows::amount(r.amount)?,
                    kind: rows::record_kind(&r.kind)?,
                    source: serde_json::from_value(json!(r.source))
                        .map_err(|_| StoreError::corrupt(format!("canal {}", r.source)))?,
                    request_id: r.request_id,
                    created_at: r.created_at,
                })
            })
            .collect()
    }

    async fn lock_auction(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        id: AuctionId,
    ) -> Result<Auction, StoreError> {
        let state = rows::load_auction(tx, id, true)
            .await?
            .ok_or(StoreError::AuctionNotFound(id))?;
        Auction::from_state(state).map_err(|e| StoreError::corrupt(e.0))
    }

    async fn close_locked(
        &self,
        mut tx: Transaction<'_, Postgres>,
        mut auction: Auction,
        now: DateTime<Utc>,
    ) -> Result<Option<Closed>, StoreError> {
        let outcome = match auction.close(now) {
            Ok(o) => o,
            Err(CloseRejection::NotDue { .. } | CloseRejection::AlreadyClosed) => {
                tx.rollback().await?;
                return Ok(None);
            }
        };
        let id = auction.id();
        let version = auction.state().version;
        let currency = auction.rules().currency;
        sqlx::query(
            "UPDATE auctions SET status = 'closed', outcome = $2, version = $3, \
             closed_at = $4, updated_at = $4 WHERE id = $1",
        )
        .bind(id.0)
        .bind(match outcome {
            Outcome::Awarded { .. } => "awarded",
            Outcome::NoSale => "no_sale",
        })
        .bind(db_i64(version)?)
        .bind(now)
        .execute(&mut *tx)
        .await?;

        let mut certificate_number = None;
        match outcome {
            Outcome::Awarded { winner, amount } => {
                // Contador con bloqueo de fila: números consecutivos sin huecos.
                let n: i64 = sqlx::query_scalar(
                    "UPDATE counters SET value = value + 1 WHERE name = 'award_certificate' \
                     RETURNING value",
                )
                .fetch_one(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO awards (auction_id, winner_id, amount, currency, \
                     certificate_number, awarded_at) VALUES ($1, $2, $3, $4, $5, $6)",
                )
                .bind(id.0)
                .bind(winner.0)
                .bind(amount.minor())
                .bind(currency.code())
                .bind(n)
                .bind(now)
                .execute(&mut *tx)
                .await?;
                certificate_number = Some(n);
                let winner_alias = auction.bidder(winner).map(|b| b.alias);
                insert_event(
                    &mut tx,
                    id,
                    version,
                    "auction.closed",
                    Audience::Public,
                    json!({
                        "auction_id": id, "version": version, "outcome": "awarded",
                        "winner_alias": winner_alias, "amount": amount,
                        "currency": currency, "server_time": now,
                    }),
                )
                .await?;
                let private = json!({
                    "auction_id": id, "version": version, "outcome": "awarded",
                    "winner": winner, "amount": amount, "currency": currency,
                    "certificate_number": n, "server_time": now,
                });
                insert_event(
                    &mut tx,
                    id,
                    version,
                    "auction.won",
                    Audience::User(winner),
                    private.clone(),
                )
                .await?;
                insert_event(
                    &mut tx,
                    id,
                    version,
                    "auction.sold",
                    Audience::User(auction.state().seller),
                    private.clone(),
                )
                .await?;
                insert_event(
                    &mut tx,
                    id,
                    version,
                    "auction.closed",
                    Audience::Admin,
                    private,
                )
                .await?;
            }
            Outcome::NoSale => {
                let payload = json!({
                    "auction_id": id, "version": version, "outcome": "no_sale",
                    "currency": currency, "server_time": now,
                });
                insert_event(
                    &mut tx,
                    id,
                    version,
                    "auction.closed",
                    Audience::Public,
                    payload.clone(),
                )
                .await?;
                insert_event(
                    &mut tx,
                    id,
                    version,
                    "auction.unsold",
                    Audience::User(auction.state().seller),
                    payload.clone(),
                )
                .await?;
                insert_event(
                    &mut tx,
                    id,
                    version,
                    "auction.closed",
                    Audience::Admin,
                    payload,
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(Some(Closed {
            auction_id: id,
            outcome,
            certificate_number,
            version,
        }))
    }
}

async fn find_request(
    conn: &mut PgConnection,
    req: &BidRequest,
) -> Result<Option<Decision>, StoreError> {
    #[derive(sqlx::FromRow)]
    struct Row {
        auction_id: Uuid,
        action: String,
        amount: i64,
        currency: String,
        decision: serde_json::Value,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT auction_id, action, amount, currency, decision FROM bid_requests \
         WHERE bidder_id = $1 AND request_id = $2",
    )
    .bind(req.command.bidder.0)
    .bind(req.request_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    let (action, amount) = rows::action_parts(req.command.action);
    if row.auction_id != req.auction_id.0
        || row.action != action
        || row.amount != amount.minor()
        || row.currency != req.command.currency.code()
    {
        return Err(StoreError::IdempotencyConflict);
    }
    serde_json::from_value(row.decision)
        .map(Some)
        .map_err(|e| StoreError::corrupt(format!("decisión guardada: {e}")))
}

async fn bidder_can_bid(conn: &mut PgConnection, bidder: UserId) -> Result<bool, StoreError> {
    // FOR SHARE: una suspensión concurrente espera a que termine esta puja.
    let ok: Option<bool> = sqlx::query_scalar(
        "SELECT status = 'active' AND can_bid FROM users WHERE id = $1 FOR SHARE",
    )
    .bind(bidder.0)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(ok.unwrap_or(false))
}

async fn record_request(
    conn: &mut PgConnection,
    req: &BidRequest,
    decision: &Decision,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    let (action, amount) = rows::action_parts(req.command.action);
    let seq = match decision {
        Decision::Accepted(a) => Some(db_i64(a.seq)?),
        Decision::Rejected { .. } => None,
    };
    let decision_json =
        serde_json::to_value(decision).map_err(|e| StoreError::corrupt(e.to_string()))?;
    let res = sqlx::query(
        "INSERT INTO bid_requests (bidder_id, request_id, auction_id, action, amount, currency, \
         source, accepted, seq, decision, received_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(req.command.bidder.0)
    .bind(req.request_id)
    .bind(req.auction_id.0)
    .bind(action)
    .bind(amount.minor())
    .bind(req.command.currency.code())
    .bind(req.source.as_str())
    .bind(seq.is_some())
    .bind(seq)
    .bind(decision_json)
    .bind(now)
    .execute(&mut *conn)
    .await;
    match res {
        Ok(_) => Ok(()),
        // La misma clave usada a la vez en otra subasta (otro bloqueo).
        Err(e) if is_request_conflict(&e) => Err(StoreError::IdempotencyConflict),
        Err(e) => Err(e.into()),
    }
}

fn is_request_conflict(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.constraint())
        .is_some_and(|c| c == "bid_requests_pkey")
}

async fn persist_applied(
    conn: &mut PgConnection,
    auction: &Auction,
    applied: &Applied,
    req: &BidRequest,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    let s = auction.state();
    sqlx::query(
        "UPDATE auctions SET closes_at = $2, leader_id = $3, price = $4, last_seq = $5, \
         version = $6, updated_at = $7 WHERE id = $1",
    )
    .bind(s.id.0)
    .bind(s.closes_at)
    .bind(applied.leader.0)
    .bind(applied.price.minor())
    .bind(db_i64(s.last_seq)?)
    .bind(db_i64(s.version)?)
    .bind(now)
    .execute(&mut *conn)
    .await?;

    for b in &applied.changed {
        sqlx::query(
            "INSERT INTO auction_bidders (auction_id, bidder_id, alias, max_amount, floor_amount, \
             shown_amount, priority_seq, has_proxy) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (auction_id, bidder_id) DO UPDATE SET max_amount = EXCLUDED.max_amount, \
             floor_amount = EXCLUDED.floor_amount, shown_amount = EXCLUDED.shown_amount, \
             priority_seq = EXCLUDED.priority_seq, has_proxy = EXCLUDED.has_proxy",
        )
        .bind(s.id.0)
        .bind(b.bidder.0)
        .bind(i32::try_from(b.alias).map_err(|_| StoreError::corrupt("alias"))?)
        .bind(b.max.minor())
        .bind(b.floor.map(Amount::minor))
        .bind(b.shown.map(Amount::minor))
        .bind(db_i64(b.priority)?)
        .bind(b.has_proxy)
        .execute(&mut *conn)
        .await?;
    }

    for r in &applied.records {
        sqlx::query(
            "INSERT INTO bids (auction_id, seq, bidder_id, amount, kind, source, request_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(s.id.0)
        .bind(db_i64(r.seq)?)
        .bind(r.bidder.0)
        .bind(r.amount.minor())
        .bind(rows::record_kind_str(r.kind))
        .bind(req.source.as_str())
        .bind(req.request_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }

    if let Action::SetMax { max } = applied.action {
        sqlx::query(
            "INSERT INTO max_bids (auction_id, seq, bidder_id, max_amount, source, request_id, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(s.id.0)
        .bind(db_i64(applied.seq)?)
        .bind(req.command.bidder.0)
        .bind(max.minor())
        .bind(req.source.as_str())
        .bind(req.request_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }

    let alias = |u: UserId| auction.bidder(u).map(|b| b.alias);
    // Evento público: alias, nunca identidades ni máximos (RF-13, §5.5).
    if !applied.records.is_empty() || applied.extended {
        insert_event(
            conn,
            s.id,
            s.version,
            "auction.bid_accepted",
            Audience::Public,
            json!({
                "auction_id": s.id,
                "version": s.version,
                "records": applied.records.iter().map(|r| json!({
                    "seq": r.seq, "alias": r.alias, "amount": r.amount, "kind": r.kind,
                })).collect::<Vec<_>>(),
                "leader_alias": alias(applied.leader),
                "price": applied.price,
                "next_min": applied.next_min,
                "closes_at": applied.closes_at,
                "extended": applied.extended,
                "server_time": now,
            }),
        )
        .await?;
    }
    for o in &applied.outbid {
        insert_event(
            conn,
            s.id,
            s.version,
            "bid.outbid",
            Audience::User(o.bidder),
            json!({
                "auction_id": s.id,
                "version": s.version,
                "price": applied.price,
                "next_min": applied.next_min,
                "closes_at": applied.closes_at,
                "max_exceeded": o.max_exceeded,
                "server_time": now,
            }),
        )
        .await?;
    }
    Ok(())
}

enum Audience {
    Public,
    User(UserId),
    Admin,
}

async fn insert_event(
    conn: &mut PgConnection,
    aggregate: AuctionId,
    version: u64,
    event_type: &str,
    audience: Audience,
    payload: serde_json::Value,
) -> Result<(), StoreError> {
    let (aud, recipient) = match audience {
        Audience::Public => ("public", None),
        Audience::User(u) => ("user", Some(u.0)),
        Audience::Admin => ("admin", None),
    };
    sqlx::query(
        "INSERT INTO outbox (aggregate_id, aggregate_version, event_type, audience, recipient_id, payload) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(aggregate.0)
    .bind(db_i64(version)?)
    .bind(event_type)
    .bind(aud)
    .bind(recipient)
    .bind(payload)
    .execute(&mut *conn)
    .await?;
    Ok(())
}
