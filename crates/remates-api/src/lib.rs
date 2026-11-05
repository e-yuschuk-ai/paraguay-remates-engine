//! API HTTP y WebSocket de Paraguay Remates.
//!
//! - Rutas y documentación OpenAPI generadas juntas con `utoipa-axum`: si una ruta
//!   existe, está documentada.
//! - `GET /openapi.json` publica el contrato; `GET /docs` lo muestra.
//! - Sin estado propio: toda decisión pasa por `remates-store` y el motor.

pub mod auth;
pub mod config;
pub mod error;
pub mod hub;
pub mod routes;
pub mod state;
pub mod workers;

use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderName, HeaderValue, Method, Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use tower::ServiceBuilder;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_scalar::{Scalar, Servable};

pub use state::AppState;

/// Límite de cuerpo de pedido: las pujas son pequeñas.
pub const BODY_LIMIT: usize = 16 * 1024;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Paraguay Remates API",
        version = "0.1.0",
        description = "API del motor de subastas. Importes en unidades menores: centavos \
            para USD, guaraníes para PYG. Ver SPEC.md y la especificación técnica v3.",
    ),
    modifiers(&SecurityAddon),
    components(schemas(routes::ws::ServerMessage, auth::Role)),
    tags(
        (name = "subastas", description = "Publicación y consulta de subastas"),
        (name = "pujas", description = "Pujas manuales, máximos automáticos e historial"),
        (name = "tiempo real", description = "WebSocket por subasta"),
        (name = "administración", description = "Requiere rol admin"),
        (name = "operación", description = "Sondas de salud"),
    )
)]
pub struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
    }
}

/// Rutas de la API junto con su documento OpenAPI.
pub fn api_router() -> (Router<AppState>, utoipa::openapi::OpenApi) {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(routes::health::health))
        .routes(routes!(routes::health::ready))
        .routes(routes!(routes::auctions::create_auction))
        .routes(routes!(routes::auctions::get_auction))
        .routes(routes!(
            routes::auctions::list_bids,
            routes::auctions::place_bid
        ))
        .routes(routes!(routes::auctions::set_max_bid))
        .routes(routes!(routes::ws::auction_ws))
        .routes(routes!(routes::admin::admin_bids))
        .routes(routes!(routes::admin::close_auction))
        .split_for_parts()
}

/// Documento OpenAPI completo, para exportarlo sin levantar el servidor.
pub fn openapi() -> utoipa::openapi::OpenApi {
    api_router().1
}

/// Aplicación completa: rutas, documentación y middleware.
pub fn app(state: AppState, cors_origins: &[HeaderValue]) -> Router {
    let (router, api) = api_router();
    let request_id = HeaderName::from_static("x-request-id");

    let mut app = router
        .route(
            "/openapi.json",
            get({
                let api = api.clone();
                move || async move { Json(api) }
            }),
        )
        .merge(Scalar::with_url("/docs", api))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(
            ServiceBuilder::new()
                .layer(SetRequestIdLayer::new(request_id.clone(), MakeRequestUuid))
                .layer(
                    TraceLayer::new_for_http().make_span_with(|req: &Request<_>| {
                        // Sólo la ruta: la query puede llevar `access_token`.
                        tracing::info_span!(
                            "http",
                            method = %req.method(),
                            path = %req.uri().path(),
                            request_id = req
                                .headers()
                                .get("x-request-id")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or(""),
                        )
                    }),
                )
                .layer(PropagateRequestIdLayer::new(request_id))
                // El WebSocket no se ve afectado: la respuesta 101 sale enseguida y la
                // sesión corre en su propia tarea. Si una puja vence por tiempo, el
                // cliente la reintenta con la misma clave y obtiene la decisión real.
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    REQUEST_TIMEOUT,
                )),
        );

    if !cors_origins.is_empty() {
        app = app.layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::list(cors_origins.iter().cloned()))
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([
                    header::AUTHORIZATION,
                    header::CONTENT_TYPE,
                    HeaderName::from_static(routes::auctions::CHANNEL_HEADER),
                    HeaderName::from_static("x-request-id"),
                ]),
        );
    }
    app.with_state(state)
}
