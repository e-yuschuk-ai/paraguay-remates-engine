//! Servidor de la API.
//!
//! `remates-api` arranca el servidor. `remates-api openapi` imprime el contrato
//! OpenAPI en JSON, sin conectarse a la base (útil en CI y para generar clientes).

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use remates_api::auth::JwtAuth;
use remates_api::config::Config;
use remates_api::hub::Hub;
use remates_api::{app, workers, AppState};
use remates_store::{Clock, Db, Store};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("openapi") {
        match serde_json::to_string_pretty(&remates_api::openapi()) {
            Ok(json) => {
                println!("{json}");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("{e}");
                return ExitCode::FAILURE;
            }
        }
    }

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuración: {e}");
            return ExitCode::FAILURE;
        }
    };
    init_tracing(config.log_json);
    match run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "el servidor terminó con error");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing(json: bool) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,sqlx=warn"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}

async fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let auth = JwtAuth::hs256(config.jwt_secret.as_bytes(), config.jwt_issuer.clone())?;
    let db = Db::connect(&config.db).await?;
    if config.run_migrations {
        db.migrate().await?;
        tracing::info!("migraciones aplicadas");
    }
    let store = Store::new(db.clone(), Clock::Database);
    let hub = Hub::new(256);
    let shutdown = CancellationToken::new();

    let mut tasks = vec![hub.start(db.clone(), shutdown.clone()).await?];
    if config.run_workers {
        tasks.push(tokio::spawn(workers::run_closer(
            store.clone(),
            Duration::from_secs(1),
            shutdown.clone(),
        )));
    }

    let state = AppState {
        store,
        auth: Arc::new(auth),
        hub,
    };
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(addr = %config.bind, "escuchando");
    let server_shutdown = shutdown.clone();
    axum::serve(listener, app(state, &config.cors_origins))
        .with_graceful_shutdown(async move {
            wait_for_signal().await;
            tracing::info!("apagando");
            server_shutdown.cancel();
        })
        .await?;
    shutdown.cancel();
    for t in tasks {
        let _ = t.await;
    }
    Ok(())
}

async fn wait_for_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}
