//! Pruebas contra PostgreSQL real. Requieren `DATABASE_URL`; `sqlx::test` crea una
//! base nueva por prueba y aplica las migraciones.

use std::collections::HashSet;

use chrono::{DateTime, TimeDelta, Utc};
use remates_engine::{
    Action, Amount, AuctionId, AuctionKind, Command, Currency, ExtensionPolicy, FirstBidRule,
    Outcome, Rejection, Rules, UserId,
};
use remates_store::outbox::{self, OutboxEvent};
use remates_store::{
    BidRequest, Clock, Db, Decision, NewAuction, RejectReason, Source, Store, StoreError,
};
use sqlx::PgPool;
use uuid::Uuid;

fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_830_000_000, 0).expect("valid timestamp")
}

fn amt(v: i64) -> Amount {
    Amount::new(v).expect("valid amount")
}

struct Fx {
    store: Store,
    clock: Clock,
    pool: PgPool,
}

/// Reloj manual en t0 + 10 s; subastas abiertas de t0 a t0 + 1000 s.
fn fx(pool: PgPool) -> Fx {
    let clock = Clock::manual(t0() + TimeDelta::seconds(10));
    Fx {
        store: Store::new(Db::new(pool.clone(), None), clock.clone()),
        clock,
        pool,
    }
}

async fn user(pool: &PgPool, can_bid: bool) -> UserId {
    let id = UserId::new();
    sqlx::query("INSERT INTO users (id, can_bid) VALUES ($1, $2)")
        .bind(id.0)
        .bind(can_bid)
        .execute(pool)
        .await
        .expect("insert user");
    id
}

async fn auction(f: &Fx, kind: AuctionKind) -> AuctionId {
    let seller = user(&f.pool, false).await;
    let id = AuctionId::new();
    f.store
        .create_auction(NewAuction {
            id,
            seller,
            rules: Rules {
                currency: Currency::Pyg,
                kind,
                increment: amt(10_000),
                opens_at: t0(),
                scheduled_close_at: t0() + TimeDelta::seconds(1000),
                extension: ExtensionPolicy::default(),
                first_bid: FirstBidRule::Increment,
            },
        })
        .await
        .expect("create auction");
    id
}

fn req(auction: AuctionId, bidder: UserId, action: Action) -> BidRequest {
    BidRequest {
        request_id: Uuid::now_v7(),
        auction_id: auction,
        command: Command {
            bidder,
            currency: Currency::Pyg,
            action,
        },
        source: Source::Web,
    }
}

fn bid(auction: AuctionId, bidder: UserId, v: i64) -> BidRequest {
    req(auction, bidder, Action::Bid { amount: amt(v) })
}

fn accepted(d: &Decision) -> bool {
    matches!(d, Decision::Accepted(_))
}

async fn events(pool: &PgPool) -> Vec<OutboxEvent> {
    sqlx::query_as(
        "SELECT id, aggregate_id, aggregate_version, event_type, audience, recipient_id, \
         payload, created_at FROM outbox ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .expect("outbox")
}

#[sqlx::test(migrations = "./migrations")]
async fn bid_is_persisted_and_visible(pool: PgPool) {
    let f = fx(pool);
    let a = auction(&f, AuctionKind::Hammer).await;
    let (u1, u2) = (user(&f.pool, true).await, user(&f.pool, true).await);

    let r = f.store.submit(bid(a, u1, 50_000)).await.expect("submit");
    assert!(accepted(&r.decision) && !r.replayed);
    let r = f
        .store
        .submit(req(a, u2, Action::SetMax { max: amt(200_000) }))
        .await
        .expect("submit");
    let Decision::Accepted(acc) = r.decision else {
        panic!("expected accepted: {r:?}")
    };
    assert!(acc.is_leader);
    assert_eq!(acc.price, amt(60_000));

    let s = f.store.standing(a).await.expect("standing");
    assert_eq!(s.leader_alias, Some(2));
    assert_eq!(s.price, Some(amt(60_000)));
    assert_eq!(s.next_min, Some(amt(70_000)));

    let h = f.store.public_history(a).await.expect("history");
    let h: Vec<(u64, u32, i64)> = h
        .iter()
        .map(|b| (b.seq, b.alias, b.amount.minor()))
        .collect();
    // seq 2 es el registro privado del máximo (tabla max_bids), no aparece.
    assert_eq!(h, vec![(1, 1, 50_000), (3, 2, 60_000)]);

    let max_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM max_bids")
        .fetch_one(&f.pool)
        .await
        .expect("count");
    assert_eq!(max_rows, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn retry_with_same_key_returns_same_decision(pool: PgPool) {
    let f = fx(pool);
    let a = auction(&f, AuctionKind::Hammer).await;
    let u = user(&f.pool, true).await;
    let r = bid(a, u, 50_000);

    let first = f.store.submit(r).await.expect("submit");
    let again = f.store.submit(r).await.expect("submit");
    assert!(again.replayed);
    assert_eq!(first.decision, again.decision);

    let bids: i64 = sqlx::query_scalar("SELECT count(*) FROM bids")
        .fetch_one(&f.pool)
        .await
        .expect("count");
    assert_eq!(bids, 1, "el reintento no duplica la puja (CA-07)");

    // Misma clave, otro importe: es un error del cliente, no una puja nueva.
    let mut other = r;
    other.command.action = Action::Bid {
        amount: amt(90_000),
    };
    assert!(matches!(
        f.store.submit(other).await,
        Err(StoreError::IdempotencyConflict)
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_retries_of_same_request_apply_once(pool: PgPool) {
    let f = fx(pool);
    let a = auction(&f, AuctionKind::Hammer).await;
    let u = user(&f.pool, true).await;
    let r = bid(a, u, 50_000);
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let store = f.store.clone();
        tasks.push(tokio::spawn(async move { store.submit(r).await }));
    }
    let mut fresh = 0;
    for t in tasks {
        let s = t.await.expect("join").expect("submit");
        assert!(accepted(&s.decision));
        if !s.replayed {
            fresh += 1;
        }
    }
    assert_eq!(fresh, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn ineligible_bidders_are_rejected_and_recorded(pool: PgPool) {
    let f = fx(pool);
    let a = auction(&f, AuctionKind::Hammer).await;
    let unverified = user(&f.pool, false).await;
    let unknown = UserId::new();
    for u in [unverified, unknown] {
        let r = f.store.submit(bid(a, u, 50_000)).await.expect("submit");
        assert_eq!(
            r.decision,
            Decision::Rejected {
                reason: RejectReason::BidderNotEligible
            }
        );
    }
    let suspended = user(&f.pool, true).await;
    sqlx::query("UPDATE users SET status = 'suspended' WHERE id = $1")
        .bind(suspended.0)
        .execute(&f.pool)
        .await
        .expect("suspend");
    let r = f
        .store
        .submit(bid(a, suspended, 50_000))
        .await
        .expect("submit");
    assert!(!accepted(&r.decision));

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM bid_requests WHERE NOT accepted")
        .fetch_one(&f.pool)
        .await
        .expect("count");
    assert_eq!(n, 3);
}

/// CA-04: muchas ofertas simultáneas del mismo importe → una sola aceptada, un líder.
#[sqlx::test(migrations = "./migrations")]
async fn simultaneous_same_amount_has_single_winner(pool: PgPool) {
    let f = fx(pool);
    let a = auction(
        &f,
        AuctionKind::Reserve {
            base: amt(1_000_000),
        },
    )
    .await;
    let mut users = Vec::new();
    for _ in 0..25 {
        users.push(user(&f.pool, true).await);
    }
    let mut tasks = Vec::new();
    for &u in &users {
        let store = f.store.clone();
        tasks.push(tokio::spawn(async move {
            store.submit(bid(a, u, 1_000_000)).await
        }));
    }
    let mut winners = Vec::new();
    for (t, u) in tasks.into_iter().zip(&users) {
        match t.await.expect("join").expect("submit").decision {
            Decision::Accepted(_) => winners.push(*u),
            Decision::Rejected {
                reason:
                    RejectReason::Engine {
                        rejection: Rejection::BelowMinimum { min },
                    },
            } => assert_eq!(min, amt(1_010_000)),
            other => panic!("decisión inesperada: {other:?}"),
        }
    }
    assert_eq!(winners.len(), 1);
    let s = f.store.standing(a).await.expect("standing");
    assert_eq!(s.price, Some(amt(1_000_000)));
    let h = f.store.admin_history(a).await.expect("history");
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].bidder, winners[0]);
}

/// Carrera con importes y máximos variados: el estado final es coherente y el
/// historial tiene un orden único y sin huecos.
#[sqlx::test(migrations = "./migrations")]
async fn concurrent_mixed_actions_keep_invariants(pool: PgPool) {
    let f = fx(pool);
    let a = auction(&f, AuctionKind::Hammer).await;
    let mut users = Vec::new();
    for _ in 0..8 {
        users.push(user(&f.pool, true).await);
    }
    let mut tasks = Vec::new();
    for round in 0..6i64 {
        for (i, &u) in users.iter().enumerate() {
            let store = f.store.clone();
            let i = i as i64;
            tasks.push(tokio::spawn(async move {
                let action = if (i + round) % 3 == 0 {
                    Action::SetMax {
                        max: amt(100_000 + 37_000 * (i + 1) * (round + 1)),
                    }
                } else {
                    Action::Bid {
                        amount: amt(10_000 * (1 + i + 8 * round)),
                    }
                };
                store.submit(req(a, u, action)).await
            }));
        }
    }
    let mut accepted_n = 0;
    for t in tasks {
        if accepted(&t.await.expect("join").expect("submit").decision) {
            accepted_n += 1;
        }
    }
    assert!(accepted_n > 0);

    // El estado guardado pasa la verificación de invariantes del motor.
    let s = f
        .store
        .standing(a)
        .await
        .expect("standing loads and validates");
    let h = f.store.public_history(a).await.expect("history");
    for w in h.windows(2) {
        assert!(w[0].seq < w[1].seq);
    }
    let top = h.iter().map(|b| b.amount).max();
    assert_eq!(
        top, s.price,
        "el mayor importe del historial es el precio vigente"
    );
    let last_seq: i64 = sqlx::query_scalar("SELECT last_seq FROM auctions WHERE id = $1")
        .bind(a.0)
        .fetch_one(&f.pool)
        .await
        .expect("last_seq");
    let used: i64 =
        sqlx::query_scalar("SELECT (SELECT count(*) FROM bids) + (SELECT count(*) FROM max_bids)")
            .fetch_one(&f.pool)
            .await
            .expect("count");
    assert_eq!(
        last_seq, used,
        "cada número de orden se usó exactamente una vez"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn late_bid_extends_and_close_awards_with_certificate(pool: PgPool) {
    let f = fx(pool);
    let a = auction(&f, AuctionKind::Hammer).await;
    let (u1, u2) = (user(&f.pool, true).await, user(&f.pool, true).await);
    f.store.submit(bid(a, u1, 50_000)).await.expect("submit");

    f.clock.set(t0() + TimeDelta::seconds(950));
    let r = f.store.submit(bid(a, u2, 60_000)).await.expect("submit");
    let Decision::Accepted(acc) = r.decision else {
        panic!("expected accepted")
    };
    assert!(acc.extended);
    assert_eq!(acc.closes_at, t0() + TimeDelta::seconds(1070));

    f.clock.set(t0() + TimeDelta::seconds(1000));
    assert!(
        f.store.close_due(10).await.expect("close").is_empty(),
        "alargada"
    );
    let r = f.store.submit(bid(a, u1, 70_000)).await.expect("submit");
    assert!(accepted(&r.decision), "todavía abierta por el alargue");

    f.clock.set(t0() + TimeDelta::seconds(2000));
    let closed = f.store.close_due(10).await.expect("close");
    assert_eq!(closed.len(), 1);
    assert_eq!(
        closed[0].outcome,
        Outcome::Awarded {
            winner: u1,
            amount: amt(70_000)
        }
    );
    assert_eq!(closed[0].certificate_number, Some(1));

    assert_eq!(
        f.store.close_auction(a).await.expect("close"),
        None,
        "una sola vez"
    );
    let r = f.store.submit(bid(a, u2, 90_000)).await.expect("submit");
    assert_eq!(
        r.decision,
        Decision::Rejected {
            reason: RejectReason::Engine {
                rejection: Rejection::Closed
            }
        }
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn parallel_closers_close_each_auction_once(pool: PgPool) {
    let f = fx(pool);
    let mut ids = Vec::new();
    for i in 0..12 {
        let a = auction(&f, AuctionKind::Hammer).await;
        if i % 3 != 0 {
            let u = user(&f.pool, true).await;
            f.store.submit(bid(a, u, 50_000)).await.expect("submit");
        }
        ids.push(a);
    }
    f.clock.set(t0() + TimeDelta::seconds(5000));
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let store = f.store.clone();
        tasks.push(tokio::spawn(async move { store.close_due(100).await }));
    }
    let mut closed = Vec::new();
    for t in tasks {
        closed.extend(t.await.expect("join").expect("close"));
    }
    let unique: HashSet<_> = closed.iter().map(|c| c.auction_id).collect();
    assert_eq!(closed.len(), 12);
    assert_eq!(unique.len(), 12);
    let mut certs: Vec<i64> = closed.iter().filter_map(|c| c.certificate_number).collect();
    certs.sort_unstable();
    assert_eq!(certs, (1..=8).collect::<Vec<_>>(), "N001…N008, sin huecos");
    let no_sale = closed
        .iter()
        .filter(|c| c.outcome == Outcome::NoSale)
        .count();
    assert_eq!(no_sale, 4);
}

#[sqlx::test(migrations = "./migrations")]
async fn outbox_events_are_safe_and_relayed(pool: PgPool) {
    let f = fx(pool);
    let a = auction(&f, AuctionKind::Hammer).await;
    let (u1, u2) = (user(&f.pool, true).await, user(&f.pool, true).await);
    f.store
        .submit(req(
            a,
            u1,
            Action::SetMax {
                max: amt(987_650_000),
            },
        ))
        .await
        .expect("submit");
    f.store.submit(bid(a, u2, 50_000)).await.expect("submit");
    f.store
        .submit(bid(a, u2, 987_650_000))
        .await
        .expect("submit"); // empate: sigue u1
    f.store
        .submit(bid(a, u2, 987_660_000))
        .await
        .expect("submit"); // u2 pasa al frente

    let ev = events(&f.pool).await;
    let public: Vec<_> = ev.iter().filter(|e| e.audience == "public").collect();
    assert!(!public.is_empty());
    for e in &public {
        let text = e.payload.to_string();
        for u in [u1, u2] {
            assert!(
                !text.contains(&u.0.to_string()),
                "evento público con identidad: {text}"
            );
        }
    }
    // El máximo privado de u1 no aparece en eventos públicos antes de agotarse.
    let first_public = public[0].payload.to_string();
    assert!(!first_public.contains("987650000"));

    let outbid: Vec<_> = ev.iter().filter(|e| e.event_type == "bid.outbid").collect();
    assert_eq!(outbid.len(), 1);
    assert_eq!(outbid[0].recipient_id, Some(u1.0));
    assert_eq!(outbid[0].payload["max_exceeded"], true);

    let decisions = ev.iter().filter(|e| e.event_type == "bid.decision").count();
    assert_eq!(
        decisions, 4,
        "una decisión por pedido, para responder por su canal"
    );

    let db = f.store.db().clone();
    let mut seen = 0;
    let n = outbox::relay_batch(&db, 100, async |batch: &[OutboxEvent]| {
        seen = batch.len();
        Ok::<_, std::convert::Infallible>(())
    })
    .await
    .expect("relay");
    assert_eq!(n, seen);
    assert_eq!(n, ev.len());
    let again = outbox::relay_batch(&db, 100, async |_: &[OutboxEvent]| {
        Ok::<_, std::convert::Infallible>(())
    })
    .await
    .expect("relay");
    assert_eq!(again, 0);

    // Si el publicador falla, el lote queda pendiente.
    f.store
        .submit(bid(a, u1, 990_000_000))
        .await
        .expect("submit");
    let failed = outbox::relay_batch(&db, 100, async |_: &[OutboxEvent]| Err("caído")).await;
    assert!(failed.is_err());
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox WHERE published_at IS NULL")
        .fetch_one(&f.pool)
        .await
        .expect("count");
    assert!(pending > 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn history_is_append_only(pool: PgPool) {
    let f = fx(pool);
    let a = auction(&f, AuctionKind::Hammer).await;
    let u = user(&f.pool, true).await;
    f.store.submit(bid(a, u, 50_000)).await.expect("submit");
    for sql in [
        "UPDATE bids SET amount = 1",
        "DELETE FROM bids",
        "DELETE FROM bid_requests",
    ] {
        assert!(sqlx::query(sql).execute(&f.pool).await.is_err(), "{sql}");
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn database_clock_is_used_by_default(pool: PgPool) {
    // Con el reloj de la base, una subasta creada para "ahora" acepta pujas.
    let store = Store::new(Db::new(pool.clone(), None), Clock::Database);
    let seller = user(&pool, false).await;
    let u = user(&pool, true).await;
    let now = Utc::now();
    let id = AuctionId::new();
    store
        .create_auction(NewAuction {
            id,
            seller,
            rules: Rules {
                currency: Currency::Usd,
                kind: AuctionKind::Hammer,
                increment: amt(100),
                opens_at: now - TimeDelta::minutes(1),
                scheduled_close_at: now + TimeDelta::days(7),
                extension: ExtensionPolicy::default(),
                first_bid: FirstBidRule::Increment,
            },
        })
        .await
        .expect("create");
    let mut r = bid(id, u, 100);
    r.command.currency = Currency::Usd;
    let s = store.submit(r).await.expect("submit");
    assert!(accepted(&s.decision), "{s:?}");
}
