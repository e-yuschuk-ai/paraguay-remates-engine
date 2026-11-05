//! Autenticación con JWT (HS256).
//!
//! Los tokens los emite la Parte 2 (identidad y roles). Esta capa sólo los verifica
//! y no confía en ningún dato de identidad que venga en el cuerpo del pedido: el
//! postor siempre es el `sub` del token.
//!
//! El rol del token habilita *rutas*. La habilitación real para pujar (identidad
//! verificada, 2FA, suspensión) se vuelve a comprobar en la base dentro de la misma
//! transacción de la puja, así que un token viejo no alcanza para pujar si el
//! usuario fue suspendido.

use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use remates_engine::UserId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

/// Longitud mínima del secreto HS256.
pub const MIN_SECRET_LEN: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Bidder,
    /// Vendedor aprobado (RF-03).
    Seller,
    Admin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    #[serde(default)]
    pub roles: Vec<Role>,
    pub iss: String,
    pub exp: i64,
}

/// Verificador (y emisor, para desarrollo y pruebas) de tokens.
pub struct JwtAuth {
    decoding: DecodingKey,
    encoding: EncodingKey,
    validation: Validation,
    issuer: String,
}

#[derive(Debug, thiserror::Error)]
#[error("JWT_SECRET debe tener al menos {MIN_SECRET_LEN} bytes")]
pub struct WeakSecret;

impl JwtAuth {
    pub fn hs256(secret: &[u8], issuer: impl Into<String>) -> Result<Self, WeakSecret> {
        if secret.len() < MIN_SECRET_LEN {
            return Err(WeakSecret);
        }
        let issuer = issuer.into();
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[issuer.as_str()]);
        validation.set_required_spec_claims(&["exp", "sub", "iss"]);
        validation.leeway = 30;
        Ok(Self {
            decoding: DecodingKey::from_secret(secret),
            encoding: EncodingKey::from_secret(secret),
            validation,
            issuer,
        })
    }

    pub fn verify(&self, token: &str) -> Result<AuthUser, ApiError> {
        let data = jsonwebtoken::decode::<Claims>(token, &self.decoding, &self.validation)
            .map_err(|_| ApiError::Unauthorized)?;
        Ok(AuthUser {
            id: UserId(data.claims.sub),
            roles: data.claims.roles,
        })
    }

    /// Emite un token. Sólo para desarrollo y pruebas: en producción los emite la Parte 2.
    pub fn issue(&self, user: UserId, roles: &[Role], ttl: chrono::TimeDelta) -> String {
        let claims = Claims {
            sub: user.0,
            roles: roles.to_vec(),
            iss: self.issuer.clone(),
            exp: (chrono::Utc::now() + ttl).timestamp(),
        };
        // Codificar HS256 con una clave válida no falla.
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .unwrap_or_default()
    }
}

/// Usuario autenticado por `Authorization: Bearer <token>`.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: UserId,
    pub roles: Vec<Role>,
}

impl AuthUser {
    pub fn has(&self, role: Role) -> bool {
        self.roles.contains(&role)
    }

    pub fn require(&self, role: Role) -> Result<(), ApiError> {
        if self.has(role) {
            Ok(())
        } else {
            Err(ApiError::Forbidden)
        }
    }
}

pub(crate) fn bearer(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = bearer(parts).ok_or(ApiError::Unauthorized)?;
        state.auth.verify(token)
    }
}

/// Usuario autenticado con rol de administrador.
#[derive(Debug, Clone)]
pub struct AdminUser(pub AuthUser);

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        user.require(Role::Admin)?;
        Ok(AdminUser(user))
    }
}
