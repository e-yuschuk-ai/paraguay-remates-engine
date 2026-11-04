//! Reloj autoritativo (RT-03).

use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::PgConnection;

/// De dónde sale "ahora".
///
/// En producción es el reloj de la base primaria (`clock_timestamp()`), leído
/// después de tomar el bloqueo de la subasta: todas las instancias del backend
/// comparten así un único reloj, sin depender de la hora de cada servidor.
#[derive(Debug, Clone, Default)]
pub enum Clock {
    #[default]
    Database,
    /// Reloj manual, sólo para pruebas.
    Manual(Arc<Mutex<DateTime<Utc>>>),
}

impl Clock {
    pub fn manual(start: DateTime<Utc>) -> Self {
        Clock::Manual(Arc::new(Mutex::new(start)))
    }

    /// Fija el reloj manual. No hace nada con el reloj de la base.
    pub fn set(&self, to: DateTime<Utc>) {
        if let Clock::Manual(m) = self {
            *m.lock().unwrap_or_else(|e| e.into_inner()) = to;
        }
    }

    /// Avanza el reloj manual.
    pub fn advance(&self, by: TimeDelta) {
        if let Clock::Manual(m) = self {
            let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
            *g += by;
        }
    }

    pub(crate) async fn now(&self, conn: &mut PgConnection) -> Result<DateTime<Utc>, sqlx::Error> {
        match self {
            Clock::Database => {
                sqlx::query_scalar("SELECT clock_timestamp()")
                    .fetch_one(conn)
                    .await
            }
            Clock::Manual(m) => Ok(*m.lock().unwrap_or_else(|e| e.into_inner())),
        }
    }
}
