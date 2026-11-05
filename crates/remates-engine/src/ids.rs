//! Identificadores fuertemente tipados.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        #[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(value_type = String, format = Uuid))]
        pub struct $name(pub Uuid);

        impl $name {
            /// Nuevo identificador UUIDv7 (ordenable por tiempo).
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }
    };
}

id_type!(
    /// Usuario de la plataforma (postor, vendedor o administrador).
    UserId
);
id_type!(
    /// Subasta / publicación.
    AuctionId
);
