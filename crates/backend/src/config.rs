//! Environment configuration.
//!
//! Everything is read once at boot and fails loudly if something required is
//! missing. A backend that starts without its API key would only fail later, in
//! the middle of a player's queue.

use std::env;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    Missing(&'static str),
}

#[derive(Debug, Clone)]
pub struct Config {
    /// The GameFlow gateway, including the `/v1` prefix.
    pub api_url: String,
    /// Never leaves this process.
    pub api_key: String,
    pub game_id: String,
    pub game_mode: String,
    pub region: String,
    /// Shared with the game server. Signs session tokens and guards the
    /// internal result endpoint.
    pub api_token: String,
    /// Signs player tokens. Different from `api_token` on purpose: a leak
    /// of one must not grant the other.
    pub jwt_secret: String,
    pub port: u16,
    /// How long a player waits in the queue before being told nobody was found.
    pub queue_timeout_secs: u64,
}

fn required(key: &'static str) -> Result<String, ConfigError> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or(ConfigError::Missing(key))
}

fn optional(key: &str, fallback: &str) -> String {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

impl Config {
    pub fn from_env() -> Result<Config, ConfigError> {
        Ok(Config {
            api_url: required("GAMEFLOW_API_URL")?.trim_end_matches('/').to_string(),
            api_key: required("GAMEFLOW_API_KEY")?,
            game_id: required("GAMEFLOW_GAME_ID")?,
            game_mode: optional("GAMEFLOW_GAME_MODE", "1v1"),
            region: optional("GAMEFLOW_REGION", "us-east"),
            api_token: required("GAME_BACKEND_API_TOKEN")?,
            jwt_secret: required("JWT_SECRET")?,
            port: optional("PORT", "8080").parse().unwrap_or(8080),
            queue_timeout_secs: optional("QUEUE_TIMEOUT_SECONDS", "90")
                .parse()
                .unwrap_or(90),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_variable_names_itself() {
        let err = required("GHOSTCHASE_DEFINITELY_NOT_SET").unwrap_err();
        assert_eq!(err.to_string(), "missing required environment variable GHOSTCHASE_DEFINITELY_NOT_SET");
    }

    #[test]
    fn optional_falls_back() {
        assert_eq!(optional("GHOSTCHASE_DEFINITELY_NOT_SET", "fallback"), "fallback");
    }
}
