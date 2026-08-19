//! The GameFlow gateway client.
//!
//! Field names here are camelCase because that is what the grpc-gateway emits.
//! They were taken from `swagger/apidocs.swagger.json` in the platform repo
//! rather than guessed, so a rename upstream will show up as a compile-time
//! diff here and not as a silent runtime failure.

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{ApiError, ApiResult};

/// What every ticket carries. On a skill-model matchmaker GameFlow resolves the
/// player's real mu and sigma itself and ignores these, which is exactly why a
/// client cannot inflate its own rating. A FIFO matchmaker does not read them
/// either: it enqueues the ticket unrated. They are sent as a safe default for
/// the one shape that still reads them, a Skill Rule published without a Skill
/// Model node, which rejects a sigma of 0 or less.
pub const NEUTRAL_MU: f64 = 25.0;
pub const NEUTRAL_SIGMA: f64 = 8.333;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateTicketRequest<'a> {
    player_id: &'a str,
    game_id: &'a str,
    game_mode: &'a str,
    mu: f64,
    sigma: f64,
    preferred_az: &'a str,
    rating_bucket: f64,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTicketResponse {
    pub ticket_id: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TicketStatus {
    #[serde(default)]
    pub ticket_id: String,
    #[serde(default)]
    pub status: String,
    /// `host:port` once the match is assigned.
    #[serde(default)]
    pub connection: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportPlayer {
    pub external_player_id: String,
    pub display_name: String,
}

#[derive(Debug, Serialize)]
pub struct ReportTeam {
    pub players: Vec<ReportPlayer>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportMatchRequest<'a> {
    game_id: &'a str,
    game_mode: &'a str,
    external_match_id: &'a str,
    teams: Vec<ReportTeam>,
    /// One entry per team, lower is better. Equal values mean a draw.
    ranks: Vec<i32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResolveRatingRequest<'a> {
    game_id: &'a str,
    game_mode: &'a str,
    external_player_id: &'a str,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct StoredRating {
    #[serde(default)]
    pub external_player_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub mu: f64,
    #[serde(default)]
    pub sigma: f64,
    /// The single number worth showing a player.
    #[serde(default)]
    pub ordinal: f64,
    #[serde(default)]
    pub matches_count: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveRatingResponse {
    #[serde(default)]
    pub found: bool,
    #[serde(default)]
    pub rating: Option<StoredRating>,
}

#[derive(Clone)]
pub struct GameFlowClient {
    http: reqwest::Client,
    config: Config,
}

impl GameFlowClient {
    pub fn new(config: Config) -> GameFlowClient {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("building the HTTP client");
        GameFlowClient { http, config }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.api_url, path)
    }

    /// Every call carries the API key. This is the only place it is used.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header("X-Api-Key", &self.config.api_key)
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        req: reqwest::RequestBuilder,
    ) -> ApiResult<T> {
        let res = req
            .send()
            .await
            .map_err(|e| ApiError::Upstream(format!("request failed: {e}")))?;

        let status = res.status();
        let body = res.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(ApiError::Upstream(format!("status {status}: {body}")));
        }

        serde_json::from_str(&body)
            .map_err(|e| ApiError::Upstream(format!("decoding response: {e}; body was {body}")))
    }

    pub async fn create_ticket(&self, player_id: &str) -> ApiResult<CreateTicketResponse> {
        let body = CreateTicketRequest {
            player_id,
            game_id: &self.config.game_id,
            game_mode: &self.config.game_mode,
            mu: NEUTRAL_MU,
            sigma: NEUTRAL_SIGMA,
            preferred_az: &self.config.region,
            rating_bucket: 0.0,
            tags: Vec::new(),
        };
        self.send(self.request(reqwest::Method::POST, "/matchmaking/tickets").json(&body))
            .await
    }

    /// Long polls the ticket. `timeout_secs` is how long the gateway holds the
    /// request open before answering with whatever it has.
    pub async fn ticket_status(&self, ticket_id: &str, timeout_secs: u32) -> ApiResult<TicketStatus> {
        let path = format!("/matchmaking/tickets/{ticket_id}/status");
        self.send(
            self.request(reqwest::Method::GET, &path)
                .query(&[("timeoutSeconds", timeout_secs)]),
        )
        .await
    }

    pub async fn cancel_ticket(&self, ticket_id: &str) -> ApiResult<serde_json::Value> {
        let path = format!("/matchmaking/tickets/{ticket_id}");
        self.send(self.request(reqwest::Method::DELETE, &path)).await
    }

    /// Reports a finished match. Teams are ordered by rank, winner first.
    pub async fn report_match(
        &self,
        external_match_id: &str,
        teams: Vec<ReportTeam>,
        ranks: Vec<i32>,
    ) -> ApiResult<serde_json::Value> {
        let body = ReportMatchRequest {
            game_id: &self.config.game_id,
            game_mode: &self.config.game_mode,
            external_match_id,
            teams,
            ranks,
        };
        self.send(self.request(reqwest::Method::POST, "/skill-rating/matches:report").json(&body))
            .await
    }

    /// Model agnostic on purpose: the skill model lives on the matchmaker's
    /// Skill Model node and GameFlow resolves it. The game never carries an id.
    pub async fn resolve_rating(&self, player_id: &str) -> ApiResult<ResolveRatingResponse> {
        let body = ResolveRatingRequest {
            game_id: &self.config.game_id,
            game_mode: &self.config.game_mode,
            external_player_id: player_id,
        };
        self.send(
            self.request(reqwest::Method::POST, "/skill-rating/player-rating:resolve")
                .json(&body),
        )
        .await
    }
}

/// Builds the teams and ranks for a finished 1v1.
///
/// Two teams of one, ordered so the winner comes first. A draw is expressed as
/// two equal ranks, which is what the rating engine expects.
pub fn shape_1v1_report(
    p0: (&str, &str, u32),
    p1: (&str, &str, u32),
    winner: Option<u8>,
) -> (Vec<ReportTeam>, Vec<i32>) {
    let team = |id: &str, nick: &str| ReportTeam {
        players: vec![ReportPlayer {
            external_player_id: id.to_string(),
            display_name: nick.to_string(),
        }],
    };

    match winner {
        Some(0) => (vec![team(p0.0, p0.1), team(p1.0, p1.1)], vec![1, 2]),
        Some(_) => (vec![team(p1.0, p1.1), team(p0.0, p0.1)], vec![1, 2]),
        None => (vec![team(p0.0, p0.1), team(p1.0, p1.1)], vec![1, 1]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_winner_is_reported_first() {
        let (teams, ranks) = shape_1v1_report(("gst_a", "a", 100), ("gst_b", "b", 900), Some(1));
        assert_eq!(teams[0].players[0].external_player_id, "gst_b");
        assert_eq!(teams[1].players[0].external_player_id, "gst_a");
        assert_eq!(ranks, vec![1, 2]);
    }

    #[test]
    fn slot_zero_winning_keeps_the_natural_order() {
        let (teams, ranks) = shape_1v1_report(("gst_a", "a", 900), ("gst_b", "b", 100), Some(0));
        assert_eq!(teams[0].players[0].external_player_id, "gst_a");
        assert_eq!(ranks, vec![1, 2]);
    }

    #[test]
    fn a_draw_gets_two_equal_ranks() {
        let (teams, ranks) = shape_1v1_report(("gst_a", "a", 500), ("gst_b", "b", 500), None);
        assert_eq!(ranks, vec![1, 1], "equal ranks are how a draw is expressed");
        assert_eq!(teams.len(), 2);
    }

    #[test]
    fn every_team_has_exactly_one_player() {
        let (teams, ranks) = shape_1v1_report(("gst_a", "a", 1), ("gst_b", "b", 2), Some(1));
        assert_eq!(teams.len(), 2);
        assert_eq!(ranks.len(), teams.len(), "ranks must match teams one to one");
        for t in &teams {
            assert_eq!(t.players.len(), 1);
        }
    }

    #[test]
    fn display_names_travel_with_the_report() {
        let (teams, _) = shape_1v1_report(("gst_a", "yurei", 1), ("gst_b", "rival", 2), Some(0));
        assert_eq!(teams[0].players[0].display_name, "yurei");
        assert_eq!(teams[1].players[0].display_name, "rival");
    }

    #[test]
    fn a_ticket_serialises_with_camel_case_keys() {
        let body = CreateTicketRequest {
            player_id: "gst_a",
            game_id: "gm_1",
            game_mode: "1v1",
            mu: NEUTRAL_MU,
            sigma: NEUTRAL_SIGMA,
            preferred_az: "us-east",
            rating_bucket: 0.0,
            tags: vec![],
        };
        let json = serde_json::to_value(&body).unwrap();
        for key in ["playerId", "gameId", "gameMode", "preferredAz", "ratingBucket"] {
            assert!(json.get(key).is_some(), "missing {key} in {json}");
        }
    }

    #[test]
    fn a_report_serialises_with_camel_case_keys() {
        let (teams, ranks) = shape_1v1_report(("gst_a", "a", 1), ("gst_b", "b", 2), Some(0));
        let body = ReportMatchRequest {
            game_id: "gm_1",
            game_mode: "1v1",
            external_match_id: "mt_1",
            teams,
            ranks,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("externalMatchId").is_some());
        assert!(json["teams"][0]["players"][0]
            .get("externalPlayerId")
            .is_some());
    }
}
