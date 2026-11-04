//! Reglas de una subasta, fijadas al publicarla (RF-05 a RF-09, RF-12).

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::money::{Amount, Currency};

/// Modalidad elegida por el vendedor (DEC-05, RF-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuctionKind {
    /// Remate al martillo: sin precio base; se adjudica a la mayor oferta válida.
    Hammer,
    /// Remate con precio de reserva: el mínimo del vendedor es público y las
    /// ofertas comienzan ahí. Sin ofertas, el cierre es desierto.
    Reserve { base: Amount },
}

/// Primera oferta válida en una subasta al martillo.
///
/// **Pendiente P-04 / RT-05.** "Arrancar desde cero" no dice si cero es una oferta
/// válida. La propuesta de la v3 es que la primera oferta sea igual al incremento;
/// es el valor por defecto, pero queda configurable hasta que el cliente lo apruebe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstBidRule {
    /// La primera oferta mínima es el incremento configurado (propuesta RT-05).
    #[default]
    Increment,
    /// Cualquier importe positivo (una unidad menor) es una primera oferta válida.
    AnyPositive,
}

/// Cómo se alarga el cierre cuando entra una puja en la ventana final.
///
/// **Pendiente P-04 / RF-12.** No está acordado si cada puja reinicia los dos
/// minutos desde su recepción o suma tiempo al cierre anterior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionMode {
    /// El cierre pasa a ser `recepción + extend_by` (nunca se acorta).
    #[default]
    ResetFromBid,
    /// Se suma `extend_by` al cierre vigente.
    AddToClose,
}

/// Alargue por pujas de último momento (RF-12). Por defecto: 2 min / 2 min.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionPolicy {
    /// Si una puja entra cuando falta este tiempo o menos para el cierre, se alarga.
    #[serde(with = "millis")]
    pub window: TimeDelta,
    /// Cuánto se alarga. Cero desactiva el alargue.
    #[serde(with = "millis")]
    pub extend_by: TimeDelta,
    pub mode: ExtensionMode,
}

impl Default for ExtensionPolicy {
    fn default() -> Self {
        Self {
            window: TimeDelta::minutes(2),
            extend_by: TimeDelta::minutes(2),
            mode: ExtensionMode::default(),
        }
    }
}

impl ExtensionPolicy {
    /// Alargue desactivado.
    pub fn disabled() -> Self {
        Self {
            window: TimeDelta::zero(),
            extend_by: TimeDelta::zero(),
            mode: ExtensionMode::default(),
        }
    }

    /// Nuevo cierre si una acción visible ocurre en `now` con cierre `closes_at`.
    pub(crate) fn extended_close(
        &self,
        closes_at: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Option<DateTime<Utc>> {
        if self.extend_by <= TimeDelta::zero() || closes_at - now > self.window {
            return None;
        }
        let candidate = match self.mode {
            ExtensionMode::ResetFromBid => (now + self.extend_by).max(closes_at),
            ExtensionMode::AddToClose => closes_at + self.extend_by,
        };
        (candidate > closes_at).then_some(candidate)
    }
}

/// Reglas inmutables de una subasta.
///
/// La moneda y las reglas no cambian una vez que hay ofertas (RT-04); este motor
/// no ofrece ninguna operación para modificarlas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    pub currency: Currency,
    pub kind: AuctionKind,
    /// Escalón mínimo entre ofertas, definido por el vendedor (RF-07).
    pub increment: Amount,
    /// Apertura a pujas.
    pub opens_at: DateTime<Utc>,
    /// Cierre programado, antes de cualquier alargue.
    pub scheduled_close_at: DateTime<Utc>,
    pub extension: ExtensionPolicy,
    pub first_bid: FirstBidRule,
}

impl Rules {
    /// Verifica la coherencia de las reglas.
    pub fn validate(&self) -> Result<(), RulesError> {
        if self.scheduled_close_at <= self.opens_at {
            return Err(RulesError::CloseNotAfterOpen);
        }
        if self.extension.window < TimeDelta::zero() || self.extension.extend_by < TimeDelta::zero()
        {
            return Err(RulesError::NegativeExtension);
        }
        Ok(())
    }

    /// Importe mínimo de la primera oferta.
    pub fn opening_min(&self) -> Amount {
        match self.kind {
            AuctionKind::Reserve { base } => base,
            AuctionKind::Hammer => match self.first_bid {
                FirstBidRule::Increment => self.increment,
                FirstBidRule::AnyPositive => Amount::new(1).unwrap_or(self.increment),
            },
        }
    }
}

/// Reglas incoherentes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RulesError {
    #[error("el cierre programado debe ser posterior a la apertura")]
    CloseNotAfterOpen,
    #[error("la ventana y el alargue no pueden ser negativos")]
    NegativeExtension,
}

mod millis {
    use chrono::TimeDelta;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &TimeDelta, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i64(value.num_milliseconds())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<TimeDelta, D::Error> {
        let ms = i64::deserialize(d)?;
        TimeDelta::try_milliseconds(ms).ok_or_else(|| serde::de::Error::custom("duración inválida"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).expect("valid timestamp")
    }

    #[test]
    fn reset_from_bid_extends_from_reception() {
        let p = ExtensionPolicy::default();
        // Faltan 30 s: el cierre pasa a recepción + 2 min.
        assert_eq!(p.extended_close(t(600), t(570)), Some(t(690)));
        // Faltan 3 min: fuera de la ventana.
        assert_eq!(p.extended_close(t(600), t(420)), None);
        // Borde exacto: faltan 2 min justos → dentro de la ventana, pero no cambia nada.
        assert_eq!(p.extended_close(t(600), t(480)), None);
    }

    #[test]
    fn add_to_close_adds_to_current_close() {
        let p = ExtensionPolicy {
            mode: ExtensionMode::AddToClose,
            ..ExtensionPolicy::default()
        };
        assert_eq!(p.extended_close(t(600), t(570)), Some(t(720)));
        assert_eq!(p.extended_close(t(600), t(480)), Some(t(720)));
    }

    #[test]
    fn disabled_never_extends() {
        assert_eq!(
            ExtensionPolicy::disabled().extended_close(t(600), t(599)),
            None
        );
    }
}
