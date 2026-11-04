//! Monedas e importes exactos (RT-04).
//!
//! Los importes se representan como enteros en la unidad menor de cada moneda:
//! centavos para USD y guaraníes enteros para PYG. Nunca se usan flotantes.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Moneda de una publicación. Una subasta corre entera en una sola moneda (RF-06).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    /// Dólar estadounidense, 2 decimales (importes en centavos).
    Usd,
    /// Guaraní paraguayo, sin decimales.
    Pyg,
}

impl Currency {
    /// Cantidad de decimales de la unidad menor.
    pub const fn minor_digits(self) -> u32 {
        match self {
            Currency::Usd => 2,
            Currency::Pyg => 0,
        }
    }

    /// Código ISO 4217.
    pub const fn code(self) -> &'static str {
        match self {
            Currency::Usd => "USD",
            Currency::Pyg => "PYG",
        }
    }

    /// Interpreta un código ISO 4217 soportado.
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "USD" => Some(Currency::Usd),
            "PYG" => Some(Currency::Pyg),
            _ => None,
        }
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

/// Importe positivo en unidades menores, acotado por [`Amount::MAX`].
///
/// La moneda no viaja dentro del importe: la define la subasta. Comparar importes
/// sólo tiene sentido dentro de una misma subasta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct Amount(i64);

impl Amount {
    /// Tope para cualquier importe (10^15 unidades menores). Deja margen amplio
    /// para sumar incrementos sin desbordar `i64`.
    pub const MAX: Amount = Amount(1_000_000_000_000_000);

    /// Construye un importe; devuelve `None` si no está en `1..=MAX`.
    pub const fn new(minor: i64) -> Option<Self> {
        if minor > 0 && minor <= Self::MAX.0 {
            Some(Amount(minor))
        } else {
            None
        }
    }

    /// Valor en unidades menores.
    pub const fn minor(self) -> i64 {
        self.0
    }

    /// Suma acotada: `None` si el resultado supera [`Amount::MAX`].
    pub fn checked_add(self, other: Amount) -> Option<Amount> {
        self.0.checked_add(other.0).and_then(Amount::new)
    }
}

impl TryFrom<i64> for Amount {
    type Error = InvalidAmount;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Amount::new(value).ok_or(InvalidAmount(value))
    }
}

impl From<Amount> for i64 {
    fn from(value: Amount) -> Self {
        value.0
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Error al construir un [`Amount`] fuera de rango.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("importe fuera de rango: {0} (debe estar entre 1 y {max})", max = Amount::MAX.0)]
pub struct InvalidAmount(pub i64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_positive_and_too_large() {
        assert!(Amount::new(0).is_none());
        assert!(Amount::new(-5).is_none());
        assert!(Amount::new(Amount::MAX.minor() + 1).is_none());
        assert_eq!(Amount::new(1).map(Amount::minor), Some(1));
    }

    #[test]
    fn checked_add_respects_max() {
        let max = Amount::MAX;
        let one = Amount::new(1).expect("valid");
        assert!(max.checked_add(one).is_none());
        assert_eq!(one.checked_add(one).map(Amount::minor), Some(2));
    }

    #[test]
    fn currency_codes_roundtrip() {
        for c in [Currency::Usd, Currency::Pyg] {
            assert_eq!(Currency::from_code(c.code()), Some(c));
        }
        assert_eq!(Currency::from_code("EUR"), None);
    }

    #[test]
    fn amount_serde_validates() {
        assert!(serde_json::from_str::<Amount>("0").is_err());
        assert_eq!(
            serde_json::from_str::<Amount>("50000").ok(),
            Amount::new(50_000)
        );
    }
}
