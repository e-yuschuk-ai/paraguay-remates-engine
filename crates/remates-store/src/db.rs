//! Conexiones: primaria para escrituras y lecturas consistentes, réplica para el resto.

use std::time::Duration;

use sqlx::postgres::{PgPool, PgPoolOptions};

/// Configuración de conexión.
#[derive(Debug, Clone)]
pub struct DbConfig {
    /// Base primaria: toda escritura y toda lectura que decide algo.
    pub primary_url: String,
    /// Réplica de lectura opcional. Sin réplica, las lecturas van a la primaria.
    pub replica_url: Option<String>,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
}

impl DbConfig {
    /// Lee `DATABASE_URL`, `DATABASE_REPLICA_URL` y `DATABASE_MAX_CONNECTIONS`.
    pub fn from_env() -> Result<Self, std::env::VarError> {
        Ok(Self {
            primary_url: std::env::var("DATABASE_URL")?,
            replica_url: std::env::var("DATABASE_REPLICA_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            max_connections: std::env::var("DATABASE_MAX_CONNECTIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
            acquire_timeout: Duration::from_secs(5),
        })
    }
}

/// Pools de conexión.
///
/// Regla de uso: la réplica puede estar atrasada. Nunca se usa para validar una
/// puja, decidir un cierre ni leer lo que se acaba de escribir; sólo para vistas
/// públicas (catálogo, historial, posición informativa).
#[derive(Debug, Clone)]
pub struct Db {
    primary: PgPool,
    replica: PgPool,
}

impl Db {
    pub async fn connect(cfg: &DbConfig) -> Result<Self, sqlx::Error> {
        let opts = || {
            PgPoolOptions::new()
                .max_connections(cfg.max_connections)
                .acquire_timeout(cfg.acquire_timeout)
        };
        let primary = opts().connect(&cfg.primary_url).await?;
        let replica = match &cfg.replica_url {
            Some(url) => Some(opts().connect(url).await?),
            None => None,
        };
        Ok(Self::new(primary, replica))
    }

    pub fn new(primary: PgPool, replica: Option<PgPool>) -> Self {
        let replica = replica.unwrap_or_else(|| primary.clone());
        Self { primary, replica }
    }

    /// Aplica las migraciones pendientes en la primaria.
    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.primary).await
    }

    pub fn primary(&self) -> &PgPool {
        &self.primary
    }

    pub fn replica(&self) -> &PgPool {
        &self.replica
    }
}
