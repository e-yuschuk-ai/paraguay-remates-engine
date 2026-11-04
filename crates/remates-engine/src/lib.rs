//! Motor de subastas de Paraguay Remates.
//!
//! Dominio puro: no hace IO, no lee el reloj y no conoce la base de datos.
//! Recibe el instante del servidor como parámetro (RT-03) y devuelve los efectos
//! de cada acción para que la capa de persistencia los guarde de forma atómica
//! (RT-01) junto con sus eventos.
//!
//! Ver `SPEC.md`, Parte 4, y la especificación técnica v3 en `docs/`.

pub mod auction;
pub mod ids;
pub mod money;
pub mod rules;

pub use auction::{
    Action, Applied, Auction, AuctionState, BidRecord, BidderEntry, CloseRejection, Command,
    CorruptState, Outbid, Outcome, Phase, RecordKind, Rejection, Standing, Status,
};
pub use ids::{AuctionId, UserId};
pub use money::{Amount, Currency, InvalidAmount};
pub use rules::{AuctionKind, ExtensionMode, ExtensionPolicy, FirstBidRule, Rules, RulesError};
