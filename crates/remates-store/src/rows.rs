//! Conversión entre filas de Postgres y tipos del motor.

use chrono::{DateTime, TimeDelta, Utc};
use remates_engine::{
    Action, Amount, AuctionId, AuctionKind, AuctionState, BidderEntry, Currency, ExtensionMode,
    ExtensionPolicy, FirstBidRule, Outcome, RecordKind, Rules, Status, UserId,
};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::StoreError;

#[derive(sqlx::FromRow)]
struct AuctionRow {
    id: Uuid,
    seller_id: Uuid,
    currency: String,
    kind: String,
    base_price: Option<i64>,
    increment: i64,
    first_bid_rule: String,
    opens_at: DateTime<Utc>,
    scheduled_close_at: DateTime<Utc>,
    closes_at: DateTime<Utc>,
    extension_window_ms: i64,
    extension_ms: i64,
    extension_mode: String,
    status: String,
    outcome: Option<String>,
    leader_id: Option<Uuid>,
    price: Option<i64>,
    last_seq: i64,
    version: i64,
}

#[derive(sqlx::FromRow)]
struct BidderRow {
    bidder_id: Uuid,
    alias: i32,
    max_amount: i64,
    floor_amount: Option<i64>,
    shown_amount: Option<i64>,
    priority_seq: i64,
    has_proxy: bool,
}

const AUCTION_COLUMNS: &str = "id, seller_id, currency, kind, base_price, increment, \
    first_bid_rule, opens_at, scheduled_close_at, closes_at, extension_window_ms, \
    extension_ms, extension_mode, status, outcome, leader_id, price, last_seq, version";

/// Carga el estado completo de una subasta. Con `lock`, toma `FOR UPDATE` sobre la
/// fila de la subasta: es el bloqueo que serializa todo cambio de esa subasta (RT-01).
pub(crate) async fn load_auction(
    conn: &mut PgConnection,
    id: AuctionId,
    lock: bool,
) -> Result<Option<AuctionState>, StoreError> {
    let sql = format!(
        "SELECT {AUCTION_COLUMNS} FROM auctions WHERE id = $1{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let Some(row) = sqlx::query_as::<_, AuctionRow>(&sql)
        .bind(id.0)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(None);
    };
    let bidders = sqlx::query_as::<_, BidderRow>(
        "SELECT bidder_id, alias, max_amount, floor_amount, shown_amount, priority_seq, has_proxy \
         FROM auction_bidders WHERE auction_id = $1 ORDER BY alias",
    )
    .bind(id.0)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Some(to_state(row, bidders)?))
}

fn to_state(row: AuctionRow, bidders: Vec<BidderRow>) -> Result<AuctionState, StoreError> {
    let currency = Currency::from_code(&row.currency)
        .ok_or_else(|| StoreError::corrupt(format!("moneda {}", row.currency)))?;
    let kind = match (row.kind.as_str(), row.base_price) {
        ("hammer", None) => AuctionKind::Hammer,
        ("reserve", Some(b)) => AuctionKind::Reserve { base: amount(b)? },
        (k, _) => return Err(StoreError::corrupt(format!("modalidad {k}"))),
    };
    let first_bid = match row.first_bid_rule.as_str() {
        "increment" => FirstBidRule::Increment,
        "any_positive" => FirstBidRule::AnyPositive,
        other => {
            return Err(StoreError::corrupt(format!(
                "regla de primera oferta {other}"
            )))
        }
    };
    let mode = match row.extension_mode.as_str() {
        "reset_from_bid" => ExtensionMode::ResetFromBid,
        "add_to_close" => ExtensionMode::AddToClose,
        other => return Err(StoreError::corrupt(format!("modo de alargue {other}"))),
    };
    let ms = |v: i64| {
        TimeDelta::try_milliseconds(v).ok_or_else(|| StoreError::corrupt("duración inválida"))
    };
    let leader = row.leader_id.map(UserId);
    let price = row.price.map(amount).transpose()?;
    let status = match (row.status.as_str(), row.outcome.as_deref()) {
        ("active", None) => Status::Active,
        ("closed", Some("no_sale")) => Status::Closed {
            outcome: Outcome::NoSale,
        },
        ("closed", Some("awarded")) => match (leader, price) {
            (Some(winner), Some(amount)) => Status::Closed {
                outcome: Outcome::Awarded { winner, amount },
            },
            _ => return Err(StoreError::corrupt("adjudicada sin líder")),
        },
        (s, o) => return Err(StoreError::corrupt(format!("estado {s}/{o:?}"))),
    };
    let bidders = bidders
        .into_iter()
        .map(|b| {
            Ok(BidderEntry {
                bidder: UserId(b.bidder_id),
                alias: u32::try_from(b.alias).map_err(|_| StoreError::corrupt("alias"))?,
                max: amount(b.max_amount)?,
                floor: b.floor_amount.map(amount).transpose()?,
                shown: b.shown_amount.map(amount).transpose()?,
                priority: seq(b.priority_seq)?,
                has_proxy: b.has_proxy,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    Ok(AuctionState {
        id: AuctionId(row.id),
        seller: UserId(row.seller_id),
        rules: Rules {
            currency,
            kind,
            increment: amount(row.increment)?,
            opens_at: row.opens_at,
            scheduled_close_at: row.scheduled_close_at,
            extension: ExtensionPolicy {
                window: ms(row.extension_window_ms)?,
                extend_by: ms(row.extension_ms)?,
                mode,
            },
            first_bid,
        },
        closes_at: row.closes_at,
        status,
        last_seq: seq(row.last_seq)?,
        version: seq(row.version)?,
        leader,
        price,
        bidders,
    })
}

pub(crate) fn amount(v: i64) -> Result<Amount, StoreError> {
    Amount::new(v).ok_or_else(|| StoreError::corrupt(format!("importe {v}")))
}

pub(crate) fn seq(v: i64) -> Result<u64, StoreError> {
    u64::try_from(v).map_err(|_| StoreError::corrupt(format!("secuencia {v}")))
}

pub(crate) fn db_i64(v: u64) -> Result<i64, StoreError> {
    i64::try_from(v).map_err(|_| StoreError::corrupt(format!("secuencia {v}")))
}

pub(crate) fn kind_str(k: &AuctionKind) -> (&'static str, Option<i64>) {
    match k {
        AuctionKind::Hammer => ("hammer", None),
        AuctionKind::Reserve { base } => ("reserve", Some(base.minor())),
    }
}

pub(crate) fn first_bid_str(r: FirstBidRule) -> &'static str {
    match r {
        FirstBidRule::Increment => "increment",
        FirstBidRule::AnyPositive => "any_positive",
    }
}

pub(crate) fn mode_str(m: ExtensionMode) -> &'static str {
    match m {
        ExtensionMode::ResetFromBid => "reset_from_bid",
        ExtensionMode::AddToClose => "add_to_close",
    }
}

pub(crate) fn record_kind_str(k: RecordKind) -> &'static str {
    match k {
        RecordKind::Manual => "manual",
        RecordKind::Auto => "auto",
    }
}

pub(crate) fn record_kind(s: &str) -> Result<RecordKind, StoreError> {
    match s {
        "manual" => Ok(RecordKind::Manual),
        "auto" => Ok(RecordKind::Auto),
        other => Err(StoreError::corrupt(format!("tipo de puja {other}"))),
    }
}

pub(crate) fn action_parts(a: Action) -> (&'static str, Amount) {
    match a {
        Action::Bid { amount } => ("bid", amount),
        Action::SetMax { max } => ("set_max", max),
    }
}
