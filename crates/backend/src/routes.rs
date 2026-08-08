//! HTTP surface.
//!
//! Five things: hand out an identity, put a player in the queue, tell them when
//! a server is ready, take the match result from the game server, and read back
//! a rating. No game logic lives here.

use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::auth::{issue_player_token, issue_session_token, new_guest_id, Player};
use crate::error::{ApiError, ApiResult};
use crate::gameflow::shape_1v1_report;
use crate::AppState;

const NICK_MAX: usize = 16;
/// How long each poll waits upstream before answering. Short enough that the
/// client stays responsive, long enough that we are not hammering the gateway.
const POLL_SECONDS: u32 = 15;

fn clean_nick(raw: &str) -> ApiResult<String> {
    let nick: String = raw.trim().chars().take(NICK_MAX).collect();
    if nick.is_empty() {
        return Err(ApiError::BadRequest("nick cannot be empty".into()));
    }
    if nick.chars().any(|c| c.is_control()) {
        return Err(ApiError::BadRequest("nick contains invalid characters".into()));
    }
    Ok(nick)
}

// --- identity ----------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct GuestRequest {
    pub nick: String,
    /// Present when the player already has an id and only wants a fresh token.
    #[serde(default)]
    pub player_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GuestResponse {
    pub player_id: String,
    pub nick: String,
    pub token: String,
}

pub async fn guest(
    State(state): State<AppState>,
    Json(req): Json<GuestRequest>,
) -> ApiResult<Json<GuestResponse>> {
    let nick = clean_nick(&req.nick)?;

    // An existing id is honoured so a player keeps their rating across token
    // expiry. It is not a security boundary: guest identity never was one.
    let player_id = match req.player_id {
        Some(id) if id.starts_with("gst_") && id.len() == 20 => id,
        _ => new_guest_id(),
    };

    let token = issue_player_token(&state.config.jwt_secret, &player_id, &nick)?;
    tracing::info!(player_id, nick, "guest identity issued");

    Ok(Json(GuestResponse {
        player_id,
        nick,
        token,
    }))
}

// --- queue -------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct QueueResponse {
    pub ticket_id: String,
}

pub async fn enqueue(
    State(state): State<AppState>,
    player: Player,
) -> ApiResult<Json<QueueResponse>> {
    let ticket = state.gameflow.create_ticket(&player.id).await?;
    state
        .tickets
        .lock()
        .expect("ticket map poisoned")
        .insert(ticket.ticket_id.clone(), Instant::now());

    tracing::info!(player_id = player.id, ticket_id = ticket.ticket_id, "queued");
    Ok(Json(QueueResponse {
        ticket_id: ticket.ticket_id,
    }))
}

#[derive(Debug, Serialize)]
pub struct TicketResponse {
    /// `searching`, `assigned` or `timeout`.
    pub status: String,
    pub connection: Option<String>,
    /// Presented to the game server at connect time to prove who you are.
    pub session_token: Option<String>,
}

pub async fn poll_ticket(
    State(state): State<AppState>,
    player: Player,
    Path(ticket_id): Path<String>,
) -> ApiResult<Json<TicketResponse>> {
    let waited = state
        .tickets
        .lock()
        .expect("ticket map poisoned")
        .get(&ticket_id)
        .map(|t| t.elapsed().as_secs());

    let status = state.gameflow.ticket_status(&ticket_id, POLL_SECONDS).await?;
    let assigned = !status.connection.is_empty();

    if assigned {
        state
            .tickets
            .lock()
            .expect("ticket map poisoned")
            .remove(&ticket_id);

        let session_token =
            issue_session_token(&state.config.api_token, &player.id, &player.nick)?;

        let connection = status.connection;

        tracing::info!(
            player_id = player.id,
            ticket_id,
            connection,
            "match assigned"
        );

        return Ok(Json(TicketResponse {
            status: "assigned".into(),
            connection: Some(connection),
            session_token: Some(session_token),
        }));
    }

    // Nobody showed up in time. Give up rather than leaving the player staring
    // at a spinner forever.
    if waited.is_some_and(|w| w >= state.config.queue_timeout_secs) {
        state
            .tickets
            .lock()
            .expect("ticket map poisoned")
            .remove(&ticket_id);
        let _ = state.gameflow.cancel_ticket(&ticket_id).await;
        tracing::info!(player_id = player.id, ticket_id, "queue timed out");
        return Ok(Json(TicketResponse {
            status: "timeout".into(),
            connection: None,
            session_token: None,
        }));
    }

    Ok(Json(TicketResponse {
        status: if status.status.is_empty() {
            "searching".into()
        } else {
            status.status
        },
        connection: None,
        session_token: None,
    }))
}

pub async fn cancel(
    State(state): State<AppState>,
    player: Player,
    Path(ticket_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    state
        .tickets
        .lock()
        .expect("ticket map poisoned")
        .remove(&ticket_id);
    let _ = state.gameflow.cancel_ticket(&ticket_id).await;
    tracing::info!(player_id = player.id, ticket_id, "queue cancelled");
    Ok(Json(serde_json::json!({ "status": "cancelled" })))
}

// --- match result ------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ResultPlayer {
    pub player_id: String,
    #[serde(default)]
    pub nick: String,
    pub score: u32,
    /// False when the slot never connected or walked out. Such a match is not
    /// rated: it says nothing about anybody's skill.
    #[serde(default = "default_true")]
    pub present: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct MatchResultRequest {
    pub match_id: String,
    pub players: Vec<ResultPlayer>,
    /// Slot of the winner, or `None` on a draw.
    #[serde(default)]
    pub winner_slot: Option<u8>,
}

/// Called by the game server, never by a client. Guarded by the shared internal
/// token, and idempotent per match so a server retry cannot rate a match twice.
pub async fn match_result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<MatchResultRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let presented = headers
        .get("x-game-backend-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    if presented.is_empty() || presented != state.config.api_token {
        tracing::warn!(match_id = req.match_id, "rejected result with a bad backend API token");
        return Err(ApiError::Forbidden);
    }

    if req.players.len() != 2 {
        return Err(ApiError::BadRequest("a 1v1 result needs two players".into()));
    }

    let first_time = state
        .reported
        .lock()
        .expect("reported set poisoned")
        .insert(req.match_id.clone());

    if !first_time {
        tracing::info!(match_id = req.match_id, "duplicate result ignored");
        return Ok(Json(serde_json::json!({ "status": "already_reported" })));
    }

    if !req.players.iter().all(|p| p.present) {
        tracing::info!(match_id = req.match_id, "match not rated: a slot was absent");
        return Ok(Json(serde_json::json!({ "status": "not_rated" })));
    }

    let (p0, p1) = (&req.players[0], &req.players[1]);
    let (teams, ranks) = shape_1v1_report(
        (&p0.player_id, &p0.nick, p0.score),
        (&p1.player_id, &p1.nick, p1.score),
        req.winner_slot,
    );

    match state
        .gameflow
        .report_match(&req.match_id, teams, ranks)
        .await
    {
        Ok(_) => {
            tracing::info!(match_id = req.match_id, "match reported to skill rating");
            Ok(Json(serde_json::json!({ "status": "reported" })))
        }
        Err(e) => {
            // Let the server retry: drop the idempotency marker again.
            state
                .reported
                .lock()
                .expect("reported set poisoned")
                .remove(&req.match_id);
            Err(e)
        }
    }
}

// --- rating ------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RatingResponse {
    pub found: bool,
    pub mu: f64,
    pub sigma: f64,
    pub ordinal: f64,
    pub matches: i64,
}

pub async fn my_rating(
    State(state): State<AppState>,
    player: Player,
) -> ApiResult<Json<RatingResponse>> {
    let res = state.gameflow.resolve_rating(&player.id).await?;
    let rating = res.rating.unwrap_or_default();

    Ok(Json(RatingResponse {
        found: res.found,
        mu: rating.mu,
        sigma: rating.sigma,
        ordinal: rating.ordinal,
        matches: rating.matches_count,
    }))
}

pub async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nick_is_trimmed_and_capped() {
        assert_eq!(clean_nick("  yurei  ").unwrap(), "yurei");
        assert_eq!(clean_nick(&"x".repeat(50)).unwrap().len(), NICK_MAX);
    }

    #[test]
    fn an_empty_nick_is_rejected() {
        assert!(clean_nick("").is_err());
        assert!(clean_nick("    ").is_err());
    }

    #[test]
    fn control_characters_are_rejected() {
        assert!(clean_nick("yu\u{0}rei").is_err());
        assert!(clean_nick("yu\nrei").is_err());
    }

    #[test]
    fn unicode_nicks_survive() {
        assert_eq!(clean_nick("ñandú").unwrap(), "ñandú");
    }
}
