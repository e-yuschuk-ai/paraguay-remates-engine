//! Estado de una subasta y sus transiciones (RF-09 a RF-14, RT-01 a RT-07).
//!
//! # Modelo
//!
//! Cada postor tiene una entrada con:
//! - `max`: lo máximo que se comprometió a pagar (su mayor puja manual o su
//!   máximo automático). Es privado.
//! - `floor`: su mayor puja manual. Una puja manual compromete ese importe exacto.
//! - `shown`: el mayor importe suyo que aparece en el historial público.
//! - `priority`: orden del servidor de la acción que fijó `max`. Desempata.
//!
//! Después de cada acción aceptada la posición se recalcula en forma cerrada:
//! - **líder** = mayor `max`; en empate, menor `priority` (quien llegó primero).
//! - **precio** = `min(max_líder, segundo_max + incremento)`, o `max_líder` si hay
//!   empate de máximos; nunca menos que la puja manual del líder; sin segundo
//!   postor, la puja manual del líder o el mínimo de apertura.
//!
//! Así una puja máxima automática responde sola, sin iterar escalón por escalón,
//! y el resultado no depende del orden en que se evalúen las entradas.
//!
//! **Pendiente P-04.** La estrategia de máximos automáticos es una propuesta de
//! ingeniería (RF-11): debe validarla el cliente.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{AuctionId, UserId};
use crate::money::{Amount, Currency};
use crate::rules::{Rules, RulesError};

/// Entrada de un postor en una subasta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BidderEntry {
    pub bidder: UserId,
    /// Alias anónimo dentro de esta publicación: "Postor 1", "Postor 2"… (RF-13).
    pub alias: u32,
    /// Máximo comprometido. Privado: nunca va en eventos públicos (§5.5).
    pub max: Amount,
    /// Mayor puja manual, si hizo alguna.
    pub floor: Option<Amount>,
    /// Mayor importe visible en el historial.
    pub shown: Option<Amount>,
    /// Orden del servidor de la acción que fijó `max`.
    pub priority: u64,
    /// Si registró alguna vez una puja máxima automática.
    pub has_proxy: bool,
}

/// Resultado de un cierre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    Awarded { winner: UserId, amount: Amount },
    NoSale,
}

/// Estado persistido de la subasta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Status {
    /// Abierta o por abrir; la fase exacta depende del reloj ([`Phase`]).
    Active,
    Closed {
        outcome: Outcome,
    },
}

/// Fase de la subasta en un instante dado, según el reloj del servidor (RT-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Todavía no abrió.
    Upcoming,
    /// Acepta pujas.
    Open,
    /// Pasó la hora de cierre pero el cierre aún no se registró. No acepta pujas.
    AwaitingClose,
    Closed,
}

/// Qué pide el postor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Puja manual por un importe exacto.
    Bid { amount: Amount },
    /// Puja máxima automática: el servidor puja por el postor hasta `max` (RF-11).
    SetMax { max: Amount },
}

/// Pedido de un postor ya autenticado y habilitado.
///
/// La habilitación (identidad, 2FA, suspensión) la verifica la capa de persistencia
/// en la misma transacción; el motor sólo aplica las reglas de la subasta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub bidder: UserId,
    /// Moneda que el postor vio al pujar; debe coincidir con la de la subasta (RF-10).
    pub currency: Currency,
    pub action: Action,
}

/// Tipo de registro en el historial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// Puja manual del postor.
    Manual,
    /// Puja realizada por el servidor en nombre de un máximo automático.
    Auto,
}

/// Una línea del historial público de pujas (RF-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BidRecord {
    /// Orden del servidor, único por subasta y común a todos los canales (RF-14).
    pub seq: u64,
    pub bidder: UserId,
    pub alias: u32,
    pub amount: Amount,
    pub kind: RecordKind,
}

/// Aviso a un postor que quedó superado (RF-17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outbid {
    pub bidder: UserId,
    /// `true` si además se agotó su máximo automático ("máximo superado").
    pub max_exceeded: bool,
}

/// Efecto de una acción aceptada.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    /// Orden del servidor asignado a la acción del postor.
    pub seq: u64,
    pub action: Action,
    /// Líneas nuevas del historial, en orden. Un `SetMax` puede no generar ninguna.
    pub records: Vec<BidRecord>,
    pub previous_leader: Option<UserId>,
    pub leader: UserId,
    pub price: Amount,
    pub next_min: Amount,
    pub closes_at: DateTime<Utc>,
    pub extended: bool,
    /// Postores a avisar (no incluye a quien hizo la acción).
    pub outbid: Vec<Outbid>,
    /// Entradas de postores que cambiaron, para persistirlas.
    pub changed: Vec<BidderEntry>,
    pub version: u64,
}

/// Motivo por el que el motor rechaza una acción.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rejection {
    #[error("la subasta todavía no abrió")]
    NotOpenYet { opens_at: DateTime<Utc> },
    #[error("la subasta está cerrada")]
    Closed,
    #[error("el vendedor no puede pujar en su propia subasta")]
    SellerCannotBid,
    #[error("la subasta es en {expected}")]
    CurrencyMismatch { expected: Currency },
    #[error("la oferta mínima es {min}")]
    BelowMinimum { min: Amount },
    #[error("ya sos el líder; para subir, registrá una puja máxima")]
    AlreadyLeading,
    #[error("el nuevo máximo debe superar al actual ({current})")]
    MaxNotAboveCurrent { current: Amount },
    #[error("el importe supera el máximo admitido")]
    AmountOutOfRange,
}

/// Motivo por el que no se puede cerrar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CloseRejection {
    #[error("todavía no es la hora de cierre")]
    NotDue { closes_at: DateTime<Utc> },
    #[error("la subasta ya estaba cerrada")]
    AlreadyClosed,
}

/// Estado completo, tal como se guarda y se restaura.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuctionState {
    pub id: AuctionId,
    pub seller: UserId,
    pub rules: Rules,
    /// Cierre vigente, con alargues aplicados.
    pub closes_at: DateTime<Utc>,
    pub status: Status,
    /// Último orden del servidor asignado.
    pub last_seq: u64,
    /// Se incrementa con cada cambio aceptado (RT-06).
    pub version: u64,
    pub leader: Option<UserId>,
    pub price: Option<Amount>,
    pub bidders: Vec<BidderEntry>,
}

/// Estado restaurado que no cumple los invariantes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("estado de subasta inconsistente: {0}")]
pub struct CorruptState(pub String);

/// Una subasta. Todas las transiciones pasan por acá.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Auction {
    s: AuctionState,
}

/// Vista resumida de la posición actual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub phase: Phase,
    pub leader: Option<UserId>,
    pub leader_alias: Option<u32>,
    pub price: Option<Amount>,
    /// Próxima oferta mínima; `None` si ya no se aceptan pujas o desbordaría.
    pub next_min: Option<Amount>,
    pub closes_at: DateTime<Utc>,
    pub version: u64,
}

impl Auction {
    /// Crea una subasta nueva, sin pujas.
    pub fn new(id: AuctionId, seller: UserId, rules: Rules) -> Result<Self, RulesError> {
        rules.validate()?;
        Ok(Self {
            s: AuctionState {
                id,
                seller,
                closes_at: rules.scheduled_close_at,
                rules,
                status: Status::Active,
                last_seq: 0,
                version: 0,
                leader: None,
                price: None,
                bidders: Vec::new(),
            },
        })
    }

    /// Restaura una subasta persistida y verifica sus invariantes.
    pub fn from_state(state: AuctionState) -> Result<Self, CorruptState> {
        state
            .rules
            .validate()
            .map_err(|e| CorruptState(e.to_string()))?;
        let auction = Self { s: state };
        auction.check_invariants()?;
        Ok(auction)
    }

    pub fn state(&self) -> &AuctionState {
        &self.s
    }

    pub fn into_state(self) -> AuctionState {
        self.s
    }

    pub fn id(&self) -> AuctionId {
        self.s.id
    }

    pub fn rules(&self) -> &Rules {
        &self.s.rules
    }

    pub fn bidder(&self, bidder: UserId) -> Option<&BidderEntry> {
        self.s.bidders.iter().find(|b| b.bidder == bidder)
    }

    /// Fase según el reloj del servidor.
    pub fn phase(&self, now: DateTime<Utc>) -> Phase {
        match self.s.status {
            Status::Closed { .. } => Phase::Closed,
            Status::Active if now < self.s.rules.opens_at => Phase::Upcoming,
            Status::Active if now < self.s.closes_at => Phase::Open,
            Status::Active => Phase::AwaitingClose,
        }
    }

    /// Próxima oferta mínima, sin mirar el reloj.
    fn next_min_raw(&self) -> Option<Amount> {
        match self.s.price {
            None => Some(self.s.rules.opening_min()),
            Some(p) => p.checked_add(self.s.rules.increment),
        }
    }

    pub fn standing(&self, now: DateTime<Utc>) -> Standing {
        let phase = self.phase(now);
        Standing {
            phase,
            leader: self.s.leader,
            leader_alias: self.s.leader.and_then(|l| self.bidder(l)).map(|b| b.alias),
            price: self.s.price,
            next_min: match phase {
                Phase::Upcoming | Phase::Open => self.next_min_raw(),
                Phase::AwaitingClose | Phase::Closed => None,
            },
            closes_at: self.s.closes_at,
            version: self.s.version,
        }
    }

    /// Aplica una acción de un postor en el instante `now` del reloj del servidor.
    ///
    /// Si devuelve `Err`, el estado no cambia.
    pub fn apply(&mut self, cmd: Command, now: DateTime<Utc>) -> Result<Applied, Rejection> {
        match self.phase(now) {
            Phase::Upcoming => {
                return Err(Rejection::NotOpenYet {
                    opens_at: self.s.rules.opens_at,
                })
            }
            Phase::AwaitingClose | Phase::Closed => return Err(Rejection::Closed),
            Phase::Open => {}
        }
        if cmd.bidder == self.s.seller {
            return Err(Rejection::SellerCannotBid);
        }
        if cmd.currency != self.s.rules.currency {
            return Err(Rejection::CurrencyMismatch {
                expected: self.s.rules.currency,
            });
        }
        let next_min = self.next_min_raw().ok_or(Rejection::AmountOutOfRange)?;
        let is_leader = self.s.leader == Some(cmd.bidder);
        match cmd.action {
            Action::Bid { amount } => {
                if is_leader {
                    return Err(Rejection::AlreadyLeading);
                }
                if amount < next_min {
                    return Err(Rejection::BelowMinimum { min: next_min });
                }
            }
            Action::SetMax { max } => {
                if is_leader {
                    // El líder siempre tiene entrada: invariante verificado al restaurar.
                    let current = self
                        .bidder(cmd.bidder)
                        .map(|b| b.max)
                        .ok_or(Rejection::AmountOutOfRange)?;
                    if max <= current {
                        return Err(Rejection::MaxNotAboveCurrent { current });
                    }
                } else if max < next_min {
                    return Err(Rejection::BelowMinimum { min: next_min });
                }
            }
        }

        // Aceptada: a partir de acá no hay más rechazos.
        let before = self.s.bidders.clone();
        let previous_leader = self.s.leader;
        let previous_price = self.s.price;
        let mut seq = self.s.last_seq + 1;
        let action_seq = seq;
        let mut records = Vec::new();

        let idx = match self.s.bidders.iter().position(|b| b.bidder == cmd.bidder) {
            Some(i) => i,
            None => {
                let alias = u32::try_from(self.s.bidders.len() + 1).unwrap_or(u32::MAX);
                let initial = match cmd.action {
                    Action::Bid { amount } => amount,
                    Action::SetMax { max } => max,
                };
                self.s.bidders.push(BidderEntry {
                    bidder: cmd.bidder,
                    alias,
                    max: initial,
                    floor: None,
                    shown: None,
                    priority: action_seq,
                    has_proxy: false,
                });
                self.s.bidders.len() - 1
            }
        };
        {
            let entry = &mut self.s.bidders[idx];
            match cmd.action {
                Action::Bid { amount } => {
                    // amount >= next_min > precio >= máximo de cualquier no líder.
                    entry.max = amount;
                    entry.priority = action_seq;
                    entry.floor = Some(amount);
                    entry.shown = Some(amount);
                    records.push(BidRecord {
                        seq: action_seq,
                        bidder: entry.bidder,
                        alias: entry.alias,
                        amount,
                        kind: RecordKind::Manual,
                    });
                }
                Action::SetMax { max } => {
                    entry.max = max;
                    entry.priority = action_seq;
                    entry.has_proxy = true;
                }
            }
        }

        let (top, price) = self.compute_standing().ok_or(Rejection::AmountOutOfRange)?;
        let leader = self.s.bidders[top].bidder;

        // Historial: todo no líder queda mostrado en su máximo (agotado), en orden
        // ascendente; el líder, en el precio.
        let mut exhausted: Vec<usize> = (0..self.s.bidders.len())
            .filter(|&i| i != top && self.s.bidders[i].shown < Some(self.s.bidders[i].max))
            .collect();
        exhausted.sort_by_key(|&i| (self.s.bidders[i].max, self.s.bidders[i].priority));
        let mut newly_exhausted = Vec::new();
        for i in exhausted {
            seq += 1;
            let b = &mut self.s.bidders[i];
            b.shown = Some(b.max);
            records.push(BidRecord {
                seq,
                bidder: b.bidder,
                alias: b.alias,
                amount: b.max,
                kind: RecordKind::Auto,
            });
            newly_exhausted.push(b.bidder);
        }
        {
            let b = &mut self.s.bidders[top];
            if b.shown < Some(price) {
                seq += 1;
                b.shown = Some(price);
                records.push(BidRecord {
                    seq,
                    bidder: b.bidder,
                    alias: b.alias,
                    amount: price,
                    kind: RecordKind::Auto,
                });
            }
        }

        let visible_change = previous_leader != Some(leader) || previous_price != Some(price);
        let mut extended = false;
        if visible_change {
            if let Some(new_close) = self.s.rules.extension.extended_close(self.s.closes_at, now) {
                self.s.closes_at = new_close;
                extended = true;
            }
        }

        // Un máximo "viene de un automático" si supera la mayor puja manual.
        let from_proxy = |b: &BidderEntry| b.has_proxy && b.floor != Some(b.max);
        let mut outbid = Vec::new();
        if let Some(prev) = previous_leader.filter(|&p| p != leader && p != cmd.bidder) {
            outbid.push(Outbid {
                bidder: prev,
                max_exceeded: self.bidder(prev).is_some_and(from_proxy),
            });
        }
        for b in newly_exhausted {
            if b != cmd.bidder && Some(b) != previous_leader {
                // Un máximo que no lideraba pero seguía vigente y ahora se agotó.
                outbid.push(Outbid {
                    bidder: b,
                    max_exceeded: true,
                });
            }
        }

        self.s.leader = Some(leader);
        self.s.price = Some(price);
        self.s.last_seq = seq;
        self.s.version += 1;

        let changed = self
            .s
            .bidders
            .iter()
            .filter(|b| !before.contains(b))
            .cloned()
            .collect();

        debug_assert!(
            self.check_invariants().is_ok(),
            "{:?}",
            self.check_invariants()
        );

        Ok(Applied {
            seq: action_seq,
            action: cmd.action,
            records,
            previous_leader,
            leader,
            price,
            next_min: price
                .checked_add(self.s.rules.increment)
                .unwrap_or(Amount::MAX),
            closes_at: self.s.closes_at,
            extended,
            outbid,
            changed,
            version: self.s.version,
        })
    }

    /// Cierra la subasta si llegó la hora (RF-13, RT-07). Sólo puede cerrarse una vez.
    pub fn close(&mut self, now: DateTime<Utc>) -> Result<Outcome, CloseRejection> {
        if let Status::Closed { .. } = self.s.status {
            return Err(CloseRejection::AlreadyClosed);
        }
        if now < self.s.closes_at {
            return Err(CloseRejection::NotDue {
                closes_at: self.s.closes_at,
            });
        }
        let outcome = match (self.s.leader, self.s.price) {
            (Some(winner), Some(amount)) => Outcome::Awarded { winner, amount },
            _ => Outcome::NoSale,
        };
        self.s.status = Status::Closed { outcome };
        self.s.version += 1;
        Ok(outcome)
    }

    /// Índice del líder y precio según las entradas actuales.
    /// `None` sólo si no hay postores o si el cálculo desbordaría.
    fn compute_standing(&self) -> Option<(usize, Amount)> {
        let mut order: Vec<usize> = (0..self.s.bidders.len()).collect();
        order.sort_by(|&a, &b| {
            let (a, b) = (&self.s.bidders[a], &self.s.bidders[b]);
            b.max.cmp(&a.max).then(a.priority.cmp(&b.priority))
        });
        let top = *order.first()?;
        let t = &self.s.bidders[top];
        let price = match order.get(1) {
            Some(&second) => {
                let s_max = self.s.bidders[second].max;
                if s_max == t.max {
                    t.max
                } else {
                    s_max
                        .checked_add(self.s.rules.increment)
                        .map_or(t.max, |p| p.min(t.max))
                }
            }
            None => self.s.rules.opening_min(),
        };
        let price = t.floor.map_or(price, |f| f.max(price));
        Some((top, price))
    }

    /// Verifica los invariantes del estado. Se usa al restaurar y en pruebas.
    pub fn check_invariants(&self) -> Result<(), CorruptState> {
        let s = &self.s;
        let err = |m: &str| Err(CorruptState(m.to_owned()));
        let mut seen = std::collections::HashSet::new();
        let mut aliases = std::collections::HashSet::new();
        for b in &s.bidders {
            if !seen.insert(b.bidder) {
                return err("postor duplicado");
            }
            if !aliases.insert(b.alias) {
                return err("alias duplicado");
            }
            if b.priority == 0 || b.priority > s.last_seq {
                return err("prioridad fuera de rango");
            }
            if b.floor.is_some_and(|f| f > b.max) || b.shown.is_some_and(|v| v > b.max) {
                return err("importe mostrado o manual mayor que el máximo");
            }
            if b.bidder == s.seller {
                return err("el vendedor figura como postor");
            }
        }
        match (s.leader, s.price, self.compute_standing()) {
            (None, None, None) => {}
            (Some(l), Some(p), Some((top, price))) => {
                let t = &s.bidders[top];
                if t.bidder != l || p != price {
                    return err("líder o precio no coinciden con las entradas");
                }
                if t.shown != Some(p) {
                    return err("el líder no figura en el historial con el precio");
                }
                if p < s.rules.opening_min() {
                    return err("precio menor que el mínimo de apertura");
                }
                if s.bidders
                    .iter()
                    .any(|b| b.bidder != l && b.shown != Some(b.max))
                {
                    return err("un postor no líder no figura en su máximo");
                }
            }
            _ => return err("líder, precio y entradas incoherentes"),
        }
        if s.closes_at < s.rules.scheduled_close_at {
            return err("el cierre vigente es anterior al programado");
        }
        Ok(())
    }
}
