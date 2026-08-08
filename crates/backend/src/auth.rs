//! Guest identity.
//!
//! A player is a random id generated on first launch and kept on their disk.
//! The backend signs it into a token so the client cannot later claim to be
//! somebody else, which is what makes a skill rating meaningful at all.
//!
//! Two separate secrets are in play. `jwt_secret` signs the long lived player
//! token that the client holds. `api_token` signs the short lived session
//! token that the game server checks at connect time, and it is the only secret
//! the game server knows.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng as _;
use serde::{Deserialize, Serialize};

use crate::error::ApiError;
use crate::AppState;

const PLAYER_TOKEN_DAYS: u64 = 30;
/// Long enough to survive a slow allocation and a slow first connect, short
/// enough that a leaked token is worthless by the time anyone finds it.
const SESSION_TOKEN_MINUTES: u64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerClaims {
    pub sub: String,
    pub nick: String,
    pub exp: u64,
}

/// What the game server verifies when a client says hello.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClaims {
    pub sub: String,
    pub nick: String,
    pub exp: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `gst_` plus 16 hex characters from the OS generator. Guessing one has to be
/// hopeless: it is the only thing standing between a player and someone else's
/// rating.
pub fn new_guest_id() -> String {
    let bytes: [u8; 8] = rand::rng().random();
    let mut out = String::with_capacity(20);
    out.push_str("gst_");
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn issue_player_token(secret: &str, player_id: &str, nick: &str) -> Result<String, ApiError> {
    let claims = PlayerClaims {
        sub: player_id.to_string(),
        nick: nick.to_string(),
        exp: now_secs() + PLAYER_TOKEN_DAYS * 24 * 3600,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| ApiError::Upstream(format!("signing player token: {e}")))
}

/// Validation with no leeway. The library defaults to 60 seconds of grace on
/// `exp` to absorb clock skew between machines, but this process signs and
/// verifies its own tokens, so expired should mean expired.
pub fn strict_validation() -> Validation {
    let mut v = Validation::new(Algorithm::HS256);
    v.leeway = 0;
    v
}

pub fn verify_player_token(secret: &str, token: &str) -> Result<PlayerClaims, ApiError> {
    decode::<PlayerClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &strict_validation(),
    )
    .map(|d| d.claims)
    .map_err(|_| ApiError::Unauthorized)
}

pub fn issue_session_token(
    api_token: &str,
    player_id: &str,
    nick: &str,
) -> Result<String, ApiError> {
    let claims = SessionClaims {
        sub: player_id.to_string(),
        nick: nick.to_string(),
        exp: now_secs() + SESSION_TOKEN_MINUTES * 60,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(api_token.as_bytes()),
    )
    .map_err(|e| ApiError::Upstream(format!("signing session token: {e}")))
}

/// An authenticated player, extracted from the bearer token.
#[derive(Debug, Clone)]
pub struct Player {
    pub id: String,
    pub nick: String,
}

impl FromRequestParts<AppState> for Player {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;

        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .ok_or(ApiError::Unauthorized)?;

        let claims = verify_player_token(&state.config.jwt_secret, token.trim())?;
        Ok(Player {
            id: claims.sub,
            nick: claims.nick,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_ids_are_prefixed_and_unique() {
        let a = new_guest_id();
        let b = new_guest_id();
        assert!(a.starts_with("gst_"));
        assert_eq!(a.len(), 20);
        assert_ne!(a, b, "two guests collided, the generator is broken");
    }

    #[test]
    fn a_player_token_round_trips() {
        let token = issue_player_token("secret", "gst_abc", "yurei").unwrap();
        let claims = verify_player_token("secret", &token).unwrap();
        assert_eq!(claims.sub, "gst_abc");
        assert_eq!(claims.nick, "yurei");
    }

    #[test]
    fn a_token_signed_with_another_secret_is_rejected() {
        let token = issue_player_token("secret", "gst_abc", "yurei").unwrap();
        assert!(verify_player_token("other-secret", &token).is_err());
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(verify_player_token("secret", "not-a-token").is_err());
        assert!(verify_player_token("secret", "").is_err());
    }

    /// The game server only knows the backend API token. A player token must be
    /// useless to it, and a session token must not be forgeable with the
    /// public-facing secret.
    #[test]
    fn player_and_session_tokens_do_not_cross_validate() {
        let player = issue_player_token("jwt-secret", "gst_abc", "yurei").unwrap();
        let session = issue_session_token("internal-token", "gst_abc", "yurei").unwrap();

        assert!(verify_player_token("internal-token", &player).is_err());
        assert!(verify_player_token("jwt-secret", &session).is_err());

        // The server verifies the session token with the internal secret.
        let ok = decode::<SessionClaims>(
            &session,
            &DecodingKey::from_secret(b"internal-token"),
            &strict_validation(),
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn an_expired_token_is_rejected() {
        let claims = PlayerClaims {
            sub: "gst_abc".into(),
            nick: "yurei".into(),
            exp: now_secs() - 10,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(b"secret"),
        )
        .unwrap();
        assert!(verify_player_token("secret", &token).is_err());
    }
}
