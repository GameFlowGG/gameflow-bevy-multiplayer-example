//! The launch payload.
//!
//! GameFlow writes the allocation payload into the pod annotations and the SDK
//! hands it back through `gf.payload()`. For a queue-formed match the payload
//! is produced by the matchmaker, not by our backend, so the exact shape is not
//! ours to define. Parsing is therefore deliberately forgiving: several key
//! spellings are accepted, and a payload that cannot be understood is not fatal.
//!
//! When the roster is unusable the server falls back to assigning slots in
//! connection order. Identity is still proven by the session token, which is
//! signed with the internal secret, so a bad payload costs us seat ordering and
//! nothing more.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq)]
pub struct RosterPlayer {
    pub player_id: String,
    pub nick: String,
    pub slot: u8,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Roster {
    pub match_id: String,
    pub players: Vec<RosterPlayer>,
}

#[derive(Debug, Deserialize)]
struct RawPlayer {
    #[serde(alias = "playerId", alias = "player_id", alias = "id")]
    player_id: Option<String>,
    #[serde(alias = "displayName", alias = "display_name", alias = "name")]
    nick: Option<String>,
    slot: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct RawRoster {
    #[serde(alias = "matchId", alias = "match_id", alias = "id")]
    match_id: Option<String>,
    #[serde(alias = "players", alias = "roster", alias = "tickets")]
    players: Option<Vec<RawPlayer>>,
}

impl Roster {
    /// Parses a payload. Returns `None` when there is nothing usable in it.
    pub fn parse(payload: &str) -> Option<Roster> {
        let raw: RawRoster = serde_json::from_str(payload).ok()?;
        let players = raw.players?;

        let players: Vec<RosterPlayer> = players
            .into_iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let player_id = p.player_id?;
                if player_id.trim().is_empty() {
                    return None;
                }
                Some(RosterPlayer {
                    nick: p.nick.unwrap_or_else(|| format!("player{}", i + 1)),
                    slot: p.slot.unwrap_or(i as u8),
                    player_id,
                })
            })
            .collect();

        if players.is_empty() {
            return None;
        }

        Some(Roster {
            match_id: raw.match_id.unwrap_or_default(),
            players,
        })
    }

    /// The slot this player owns, if the roster names them.
    pub fn slot_of(&self, player_id: &str) -> Option<u8> {
        self.players
            .iter()
            .find(|p| p.player_id == player_id)
            .map(|p| p.slot)
    }

    pub fn nick_of(&self, player_id: &str) -> Option<&str> {
        self.players
            .iter()
            .find(|p| p.player_id == player_id)
            .map(|p| p.nick.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snake_case_payload_parses() {
        let raw = r#"{"match_id":"mt_1","players":[
            {"player_id":"gst_a","nick":"a","slot":0},
            {"player_id":"gst_b","nick":"b","slot":1}]}"#;
        let r = Roster::parse(raw).unwrap();
        assert_eq!(r.match_id, "mt_1");
        assert_eq!(r.players.len(), 2);
        assert_eq!(r.slot_of("gst_b"), Some(1));
        assert_eq!(r.nick_of("gst_a"), Some("a"));
    }

    #[test]
    fn a_camel_case_payload_parses() {
        let raw = r#"{"matchId":"mt_2","players":[
            {"playerId":"gst_a","displayName":"yurei"},
            {"playerId":"gst_b","displayName":"rival"}]}"#;
        let r = Roster::parse(raw).unwrap();
        assert_eq!(r.match_id, "mt_2");
        assert_eq!(r.nick_of("gst_a"), Some("yurei"));
    }

    #[test]
    fn missing_slots_fall_back_to_array_order() {
        let raw = r#"{"players":[{"playerId":"gst_a"},{"playerId":"gst_b"}]}"#;
        let r = Roster::parse(raw).unwrap();
        assert_eq!(r.slot_of("gst_a"), Some(0));
        assert_eq!(r.slot_of("gst_b"), Some(1));
    }

    #[test]
    fn missing_nicks_get_a_placeholder() {
        let raw = r#"{"players":[{"playerId":"gst_a"}]}"#;
        let r = Roster::parse(raw).unwrap();
        assert_eq!(r.nick_of("gst_a"), Some("player1"));
    }

    #[test]
    fn a_tickets_shaped_payload_parses() {
        let raw = r#"{"id":"mt_3","tickets":[{"id":"gst_a"},{"id":"gst_b"}]}"#;
        let r = Roster::parse(raw).unwrap();
        assert_eq!(r.match_id, "mt_3");
        assert_eq!(r.players.len(), 2);
    }

    #[test]
    fn an_unusable_payload_is_none_rather_than_a_panic() {
        assert!(Roster::parse("").is_none());
        assert!(Roster::parse("not json").is_none());
        assert!(Roster::parse("{}").is_none());
        assert!(Roster::parse(r#"{"players":[]}"#).is_none());
        assert!(Roster::parse(r#"{"players":[{"nick":"nameless"}]}"#).is_none());
    }

    #[test]
    fn blank_player_ids_are_dropped() {
        let raw = r#"{"players":[{"playerId":"  "},{"playerId":"gst_b"}]}"#;
        let r = Roster::parse(raw).unwrap();
        assert_eq!(r.players.len(), 1);
        assert_eq!(r.players[0].player_id, "gst_b");
    }

    #[test]
    fn an_unknown_player_has_no_slot() {
        let raw = r#"{"players":[{"playerId":"gst_a"}]}"#;
        let r = Roster::parse(raw).unwrap();
        assert_eq!(r.slot_of("gst_zzz"), None);
    }
}
