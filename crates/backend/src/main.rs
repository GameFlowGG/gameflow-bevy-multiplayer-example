//! Ghost Chase 1v1 backend.
//!
//! The only process that holds the GameFlow API key. The desktop client and the
//! dedicated game server both go through here; neither of them ever sees the
//! key, which is the whole point of the shape.

mod auth;
mod config;
mod error;
mod gameflow;
mod routes;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::routing::{delete, get, post};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use config::Config;
use gameflow::GameFlowClient;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub gameflow: Arc<GameFlowClient>,
    /// Ticket id to the moment it was created, so the queue can time out.
    pub tickets: Arc<Mutex<HashMap<String, Instant>>>,
    /// Match ids already reported, so a server retry cannot rate a match twice.
    pub reported: Arc<Mutex<HashSet<String>>>,
}

// axum needs Config by value inside the state, but handlers only ever read it.
impl std::ops::Deref for AppState {
    type Target = Config;
    fn deref(&self) -> &Config {
        &self.config
    }
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(routes::healthz))
        .route("/auth/guest", post(routes::guest))
        .route("/queue", post(routes::enqueue))
        // axum 0.8 uses braces for path parameters, not a leading colon.
        .route("/queue/{ticket_id}", get(routes::poll_ticket))
        .route("/queue/{ticket_id}", delete(routes::cancel))
        .route("/me/rating", get(routes::my_rating))
        .route("/internal/match-result", post(routes::match_result))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=warn".into()),
        )
        .init();

    let config = Config::from_env()?;
    let port = config.port;
    tracing::info!(
        game_id = config.game_id,
        game_mode = config.game_mode,
        region = config.region,
        "starting Ghost Chase backend"
    );

    let state = AppState {
        gameflow: Arc::new(GameFlowClient::new(config.clone())),
        config: Arc::new(config),
        tickets: Arc::new(Mutex::new(HashMap::new())),
        reported: Arc::new(Mutex::new(HashSet::new())),
    };

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("listening on 0.0.0.0:{port}");
    axum::serve(listener, app(state)).await?;
    Ok(())
}
