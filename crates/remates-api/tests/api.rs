//! Pruebas de la API contra PostgreSQL real (requieren `DATABASE_URL`).

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use chrono::{DateTime, TimeDelta, Utc};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use remates_api::auth::{JwtAuth, Role};
use remates_api::hub::Hub;
use remates_api::{app, AppState};
use remates_engine::UserId;
use remates_store::{Clock, Db, Store};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

const SECRET: &[u8] = b"secreto-de-pruebas-de-al-menos-32-bytes!!";

fn t0() -> DateTime<Utc> {
    DateTime::from_timestamp(1_830_000_000, 0).expect("valid timestamp")
}

struct Fx {
    app: Router,
    state: AppState,
    auth: Arc<JwtAuth>,
    clock: Clock,
    pool: PgPool,
}

fn fx(pool: PgPool) -> Fx {
    let clock = Clock::manual(t0() + TimeDelta::seconds(10));
    let auth = Arc::new(JwtAuth::hs256(SECRET, "paraguay-remates").expect("secret"));
    let state = AppState {
        store: Store::new(Db::new(pool.clone(), None), clock.clone()),
        auth: auth.clone(),
        hub: Hub::new(64),
    };
    Fx {
        app: app(state.clone(), &[]),
        state,
        auth,
        clock,
        pool,
    }
}

impl Fx {
    async fn user(&self, can_bid: bool) -> UserId {
        let id = UserId::new();
        sqlx::query("INSERT INTO users (id, can_bid) VALUES ($1, $2)")
            .bind(id.0)
            .bind(can_bid)
            .execute(&self.pool)
            .await
            .expect("insert user");
        id
    }

    fn token(&self, user: UserId, roles: &[Role]) -> String {
        self.auth.issue(user, roles, TimeDelta::hours(1))
    }

    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .expect("request");
        let res = self.app.clone().oneshot(req).await.expect("response");
        let status = res.status();
        let bytes = res.into_body().collect().await.expect("body").to_bytes();
        let value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, value)
    }

    /// Crea una subasta en PYG, abierta de t0 a t0 + 1000 s, incremento 10.000.
    async fn auction(&self, seller: UserId) -> String {
        let token = self.token(seller, &[Role::Seller]);
        let (status, body) = self
            .call(
                Method::POST,
                "/v1/auctions",
                Some(&token),
                Some(auction_body(None)),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["auction_id"].as_str().expect("id").to_owned()
    }
}

fn auction_body(seller: Option<UserId>) -> Value {
    json!({
        "seller_id": seller,
        "currency": "PYG",
        "kind": { "type": "hammer" },
        "increment": 10_000,
        "opens_at": t0(),
        "scheduled_close_at": t0() + TimeDelta::seconds(1000),
    })
}

fn bid_body(amount: i64) -> Value {
    json!({ "request_id": Uuid::now_v7(), "amount": amount, "currency": "PYG" })
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn openapi_is_served_and_complete(pool: PgPool) {
    let f = fx(pool);
    let (status, doc) = f.call(Method::GET, "/openapi.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    for path in [
        "/v1/auctions",
        "/v1/auctions/{auction_id}",
        "/v1/auctions/{auction_id}/bids",
        "/v1/auctions/{auction_id}/max-bids",
        "/v1/auctions/{auction_id}/ws",
        "/v1/admin/auctions/{auction_id}/close",
    ] {
        assert!(doc["paths"].get(path).is_some(), "falta {path}");
    }
    // Toda referencia apunta a un esquema existente.
    let schemas = &doc["components"]["schemas"];
    let text = doc.to_string();
    for part in text.split("\"#/components/schemas/").skip(1) {
        let name = part.split('"').next().expect("name");
        assert!(schemas.get(name).is_some(), "referencia rota: {name}");
    }
    let (status, _) = f.call(Method::GET, "/docs", None, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn health_and_ready(pool: PgPool) {
    let f = fx(pool);
    assert_eq!(
        f.call(Method::GET, "/health", None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        f.call(Method::GET, "/ready", None, None).await.0,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn creating_auctions_requires_seller_or_admin(pool: PgPool) {
    let f = fx(pool);
    let seller = f.user(false).await;
    let other = f.user(false).await;
    let admin = f.user(false).await;

    let (s, _) = f
        .call(Method::POST, "/v1/auctions", None, Some(auction_body(None)))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let bidder = f.token(seller, &[Role::Bidder]);
    let (s, _) = f
        .call(
            Method::POST,
            "/v1/auctions",
            Some(&bidder),
            Some(auction_body(None)),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let as_seller = f.token(seller, &[Role::Seller]);
    let (s, _) = f
        .call(
            Method::POST,
            "/v1/auctions",
            Some(&as_seller),
            Some(auction_body(Some(other))),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "un vendedor no publica en nombre de otro"
    );

    let as_admin = f.token(admin, &[Role::Admin]);
    let (s, body) = f
        .call(
            Method::POST,
            "/v1/auctions",
            Some(&as_admin),
            Some(auction_body(None)),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (s, _) = f
        .call(
            Method::POST,
            "/v1/auctions",
            Some(&as_admin),
            Some(auction_body(Some(other))),
        )
        .await;
    assert_eq!(
        s,
        StatusCode::CREATED,
        "el admin publica en nombre del vendedor (RF-05)"
    );

    let (s, _) = f
        .call(
            Method::POST,
            "/v1/auctions",
            Some(&as_admin),
            Some(auction_body(Some(UserId::new()))),
        )
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "vendedor inexistente");

    let mut bad = auction_body(None);
    bad["scheduled_close_at"] = json!(t0() - TimeDelta::seconds(1));
    let (s, _) = f
        .call(Method::POST, "/v1/auctions", Some(&as_seller), Some(bad))
        .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn bid_flow(pool: PgPool) {
    let f = fx(pool);
    let seller = f.user(false).await;
    let id = f.auction(seller).await;
    let (u1, u2) = (f.user(true).await, f.user(true).await);
    let (t1, t2) = (f.token(u1, &[Role::Bidder]), f.token(u2, &[Role::Bidder]));
    let bids = format!("/v1/auctions/{id}/bids");

    let (s, _) = f
        .call(Method::POST, &bids, None, Some(bid_body(50_000)))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = f
        .call(
            Method::POST,
            &bids,
            Some("no-es-un-jwt"),
            Some(bid_body(50_000)),
        )
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let forged = JwtAuth::hs256(
        b"otro-secreto-de-al-menos-32-bytes!!!!!!",
        "paraguay-remates",
    )
    .expect("secret")
    .issue(u1, &[Role::Bidder], TimeDelta::hours(1));
    let (s, _) = f
        .call(Method::POST, &bids, Some(&forged), Some(bid_body(50_000)))
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "firma con otro secreto");

    let body = bid_body(50_000);
    let (s, r) = f
        .call(Method::POST, &bids, Some(&t1), Some(body.clone()))
        .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["decision"]["status"], "accepted");
    assert_eq!(r["decision"]["is_leader"], true);
    assert_eq!(r["replayed"], false);

    // Reintento tras un corte: misma clave, misma decisión.
    let (_, again) = f
        .call(Method::POST, &bids, Some(&t1), Some(body.clone()))
        .await;
    assert_eq!(again["replayed"], true);
    assert_eq!(again["decision"], r["decision"]);

    // Misma clave con otro importe: conflicto.
    let mut changed = body;
    changed["amount"] = json!(90_000);
    let (s, _) = f.call(Method::POST, &bids, Some(&t1), Some(changed)).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Puja por debajo del mínimo: 200 con el rechazo y el mínimo.
    let (s, r) = f
        .call(Method::POST, &bids, Some(&t2), Some(bid_body(55_000)))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["decision"]["status"], "rejected");
    assert_eq!(
        r["decision"]["reason"]["rejection"]["type"],
        "below_minimum"
    );
    assert_eq!(r["decision"]["reason"]["rejection"]["min"], 60_000);

    // Máximo automático de u2: responde solo.
    let (_, r) = f
        .call(
            Method::POST,
            &format!("/v1/auctions/{id}/max-bids"),
            Some(&t2),
            Some(json!({ "request_id": Uuid::now_v7(), "max": 500_000, "currency": "PYG" })),
        )
        .await;
    assert_eq!(r["decision"]["status"], "accepted", "{r}");
    assert_eq!(r["decision"]["price"], 60_000);

    let (s, standing) = f
        .call(Method::GET, &format!("/v1/auctions/{id}"), None, None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(standing["price"], 60_000);
    assert_eq!(standing["leader_alias"], 2);
    assert_eq!(standing["next_min"], 70_000);
    assert_eq!(standing["phase"], "open");

    let (_, history) = f.call(Method::GET, &bids, None, None).await;
    let history = history.as_array().expect("array");
    assert_eq!(history.len(), 2);
    let text = Value::Array(history.clone()).to_string();
    assert!(
        !text.contains(&u1.0.to_string()),
        "el historial público no tiene identidades"
    );
    assert!(!text.contains("500000"), "ni máximos privados");
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn seller_and_unverified_users_cannot_bid(pool: PgPool) {
    let f = fx(pool);
    let seller = f.user(true).await;
    let id = f.auction(seller).await;
    let bids = format!("/v1/auctions/{id}/bids");

    let t = f.token(seller, &[Role::Seller, Role::Bidder]);
    let (_, r) = f
        .call(Method::POST, &bids, Some(&t), Some(bid_body(50_000)))
        .await;
    assert_eq!(
        r["decision"]["reason"]["rejection"]["type"],
        "seller_cannot_bid"
    );

    // El token dice "bidder" pero la base dice que no verificó identidad.
    let unverified = f.user(false).await;
    let t = f.token(unverified, &[Role::Bidder]);
    let (_, r) = f
        .call(Method::POST, &bids, Some(&t), Some(bid_body(50_000)))
        .await;
    assert_eq!(r["decision"]["reason"]["type"], "bidder_not_eligible");
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn unknown_auction_is_404(pool: PgPool) {
    let f = fx(pool);
    let u = f.user(true).await;
    let t = f.token(u, &[Role::Bidder]);
    let missing = Uuid::now_v7();
    for (m, uri, body) in [
        (Method::GET, format!("/v1/auctions/{missing}"), None),
        (Method::GET, format!("/v1/auctions/{missing}/bids"), None),
        (
            Method::POST,
            format!("/v1/auctions/{missing}/bids"),
            Some(bid_body(50_000)),
        ),
    ] {
        let (s, r) = f.call(m, &uri, Some(&t), body).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(r["error"], "not_found");
    }
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn admin_endpoints(pool: PgPool) {
    let f = fx(pool);
    let seller = f.user(false).await;
    let id = f.auction(seller).await;
    let u = f.user(true).await;
    let bidder = f.token(u, &[Role::Bidder]);
    let admin = f.token(f.user(false).await, &[Role::Admin]);
    f.call(
        Method::POST,
        &format!("/v1/auctions/{id}/bids"),
        Some(&bidder),
        Some(bid_body(50_000)),
    )
    .await;

    let history = format!("/v1/admin/auctions/{id}/bids");
    let (s, _) = f.call(Method::GET, &history, Some(&bidder), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, h) = f.call(Method::GET, &history, Some(&admin), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        h[0]["bidder"],
        json!(u),
        "trazabilidad con identidad real (RF-13)"
    );
    assert_eq!(h[0]["source"], "web");

    let close = format!("/v1/admin/auctions/{id}/close");
    let (_, r) = f.call(Method::POST, &close, Some(&admin), None).await;
    assert_eq!(r["closed"], false, "todavía no es la hora");
    f.clock.set(t0() + TimeDelta::seconds(5000));
    let (_, r) = f.call(Method::POST, &close, Some(&admin), None).await;
    assert_eq!(r["closed"], true);
    assert_eq!(r["outcome"]["type"], "awarded");
    assert_eq!(r["certificate_number"], 1);
}

// ---------------------------------------------------------------- WebSocket

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout esperando mensaje")
            .expect("stream cerrado")
            .expect("error de websocket");
        if let Message::Text(t) = msg {
            return serde_json::from_str(t.as_str()).expect("json");
        }
    }
}

/// Lee eventos hasta encontrar `event_type`, devolviendo todos los vistos.
async fn until(ws: &mut Ws, event_type: &str) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        let m = next_json(ws).await;
        let done = m["event_type"] == event_type;
        seen.push(m);
        if done {
            return seen;
        }
    }
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn websocket_streams_public_and_private_events(pool: PgPool) {
    let f = fx(pool);
    let shutdown = CancellationToken::new();
    f.state
        .hub
        .start(f.state.store.db().clone(), shutdown.clone())
        .await
        .expect("hub");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = f.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await });

    let seller = f.user(false).await;
    let id = f.auction(seller).await;
    let (u1, u2) = (f.user(true).await, f.user(true).await);
    let (t1, t2) = (f.token(u1, &[Role::Bidder]), f.token(u2, &[Role::Bidder]));

    let url = format!("ws://{addr}/v1/auctions/{id}/ws");
    let (mut anon, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("anon ws");
    let (mut mine, _) = tokio_tungstenite::connect_async(format!("{url}?access_token={t1}"))
        .await
        .expect("u1 ws");
    for ws in [&mut anon, &mut mine] {
        let snap = next_json(ws).await;
        assert_eq!(snap["type"], "snapshot");
        assert_eq!(snap["standing"]["phase"], "open");
        assert_eq!(snap["standing"]["version"], 0);
    }

    let bids = format!("/v1/auctions/{id}/bids");
    f.call(Method::POST, &bids, Some(&t1), Some(bid_body(50_000)))
        .await;
    f.call(Method::POST, &bids, Some(&t2), Some(bid_body(60_000)))
        .await;

    // u1 recibe lo público y lo suyo: su decisión y el aviso de oferta superada.
    let got = until(&mut mine, "bid.outbid").await;
    let types: Vec<&str> = got
        .iter()
        .filter_map(|m| m["event_type"].as_str())
        .collect();
    assert!(types.contains(&"auction.bid_accepted"));
    assert!(types.contains(&"bid.decision"));
    let outbid = got.last().expect("outbid");
    assert_eq!(outbid["audience"], "user");
    assert_eq!(outbid["payload"]["next_min"], 70_000);
    assert!(
        got.iter().all(|m| m["audience"] != "user"
            || m["payload"].to_string().contains(&id)
                && !m.to_string().contains(&u2.0.to_string())),
        "no ve eventos privados de u2"
    );

    // El anónimo ve sólo eventos públicos, con versiones crecientes.
    let mut versions = Vec::new();
    while versions.len() < 2 {
        let m = next_json(&mut anon).await;
        assert_eq!(m["audience"], "public", "{m}");
        versions.push(m["version"].as_i64().expect("version"));
    }
    assert_eq!(versions, vec![1, 2]);

    // `resync` devuelve un snapshot actualizado.
    anon.send(Message::Text("resync".into()))
        .await
        .expect("send");
    let snap = next_json(&mut anon).await;
    assert_eq!(snap["type"], "snapshot");
    assert_eq!(snap["standing"]["price"], 60_000);
    assert_eq!(snap["standing"]["version"], 2);

    shutdown.cancel();
}

#[sqlx::test(migrations = "../remates-store/migrations")]
async fn websocket_rejects_invalid_token_and_unknown_auction(pool: PgPool) {
    let f = fx(pool);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = f.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await });
    let seller = f.user(false).await;
    let id = f.auction(seller).await;

    let bad = format!("ws://{addr}/v1/auctions/{id}/ws?access_token=basura");
    let err = tokio_tungstenite::connect_async(bad)
        .await
        .expect_err("debe fallar");
    assert!(err.to_string().contains("401"), "{err}");

    let missing = format!("ws://{addr}/v1/auctions/{}/ws", Uuid::now_v7());
    let err = tokio_tungstenite::connect_async(missing)
        .await
        .expect_err("debe fallar");
    assert!(err.to_string().contains("404"), "{err}");
}
