//! Configuración por variables de entorno.

use std::net::SocketAddr;

use axum::http::HeaderValue;
use remates_store::DbConfig;

#[derive(Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub db: DbConfig,
    pub jwt_secret: String,
    pub jwt_issuer: String,
    pub run_migrations: bool,
    pub run_workers: bool,
    pub cors_origins: Vec<HeaderValue>,
    pub log_json: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("falta la variable {0}")]
    Missing(&'static str),
    #[error("valor inválido en {0}")]
    Invalid(&'static str),
}

fn flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(default)
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let bind = std::env::var("BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:8080".into())
            .parse()
            .map_err(|_| ConfigError::Invalid("BIND_ADDR"))?;
        let db = DbConfig::from_env().map_err(|_| ConfigError::Missing("DATABASE_URL"))?;
        let jwt_secret =
            std::env::var("JWT_SECRET").map_err(|_| ConfigError::Missing("JWT_SECRET"))?;
        let cors_origins = std::env::var("CORS_ALLOWED_ORIGINS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                HeaderValue::from_str(s).map_err(|_| ConfigError::Invalid("CORS_ALLOWED_ORIGINS"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            bind,
            db,
            jwt_secret,
            jwt_issuer: std::env::var("JWT_ISSUER").unwrap_or_else(|_| "paraguay-remates".into()),
            run_migrations: flag("RUN_MIGRATIONS", false),
            run_workers: flag("RUN_WORKERS", true),
            cors_origins,
            log_json: std::env::var("LOG_FORMAT").is_ok_and(|v| v == "json"),
        })
    }
}
