//! Persistencia PostgreSQL del motor de subastas.
//!
//! - [`Db`]: pool primario (escrituras y decisiones) y réplica de lectura opcional.
//! - [`Store`]: pujas, máximos automáticos y cierres, cada uno en una transacción
//!   con bloqueo por subasta, idempotencia y outbox.
//! - [`outbox`]: publicación de eventos al resto del sistema.
//!
//! Ver `docs/ARCHITECTURE.md`.

pub mod clock;
pub mod db;
pub mod error;
pub mod outbox;
mod rows;
pub mod store;

pub use clock::Clock;
pub use db::{Db, DbConfig};
pub use error::StoreError;
pub use store::{
    Accepted, AdminBid, BidRequest, Closed, Decision, NewAuction, PublicBid, PublicStanding,
    RejectReason, Source, Store, Submitted,
};
