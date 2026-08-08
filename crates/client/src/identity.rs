//! Guest identity, kept on disk.
//!
//! The first launch asks for a nick and the backend hands back an id and a
//! signed token. Both are written next to the player's other config so the
//! second launch goes straight to the queue.
//!
//! This is not a security boundary and was never meant to be: anyone can delete
//! the file and start over. What the token does buy is that a player cannot
//! claim to be *somebody else*, which is all the rating system needs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DIR_NAME: &str = "pacman-1v1";
const FILE_NAME: &str = "identity.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Identity {
    #[serde(default)]
    pub player_id: String,
    #[serde(default)]
    pub nick: String,
    #[serde(default)]
    pub token: String,
}

impl Identity {
    /// Complete enough to skip the nick screen.
    pub fn is_usable(&self) -> bool {
        !self.player_id.is_empty() && !self.token.is_empty() && !self.nick.is_empty()
    }
}

/// `$XDG_CONFIG_HOME/pacman-1v1/identity.json`, falling back to `$HOME/.config`
/// and finally to the working directory. Resolved without pulling a crate in
/// for three lines of path joining.
pub fn identity_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(DIR_NAME).join(FILE_NAME)
}

pub fn load_from(path: &Path) -> Identity {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save_to(path: &Path, identity: &Identity) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(identity)?;
    std::fs::write(path, raw)
}

pub fn load() -> Identity {
    load_from(&identity_path())
}

pub fn save(identity: &Identity) {
    if let Err(e) = save_to(&identity_path(), identity) {
        // Not fatal: the player just gets asked for a nick again next launch.
        bevy::log::warn!("could not save the identity file: {e}");
    }
}

/// A stable u64 for the netcode handshake, derived from the player id. Netcode
/// needs a numeric client id; the real identity check is the signed token.
pub fn client_id_of(player_id: &str) -> u64 {
    // FNV-1a. Collisions only matter within one two-player match.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in player_id.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("pacman-identity-{name}.json"))
    }

    #[test]
    fn a_missing_file_yields_an_unusable_identity() {
        let id = load_from(Path::new("/definitely/not/here.json"));
        assert!(!id.is_usable());
        assert!(id.player_id.is_empty());
    }

    #[test]
    fn an_identity_round_trips_through_disk() {
        let path = temp_path("roundtrip");
        let _ = std::fs::remove_file(&path);

        let saved = Identity {
            player_id: "gst_7f3a".into(),
            nick: "yurei".into(),
            token: "eyJ".into(),
        };
        save_to(&path, &saved).unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.player_id, saved.player_id);
        assert_eq!(loaded.nick, saved.nick);
        assert!(loaded.is_usable());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_file_does_not_panic() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(!load_from(&path).is_usable());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_partial_identity_is_not_usable() {
        let path = temp_path("partial");
        std::fs::write(&path, r#"{"player_id":"gst_a"}"#).unwrap();
        let id = load_from(&path);
        assert_eq!(id.player_id, "gst_a");
        assert!(!id.is_usable(), "no token means we still need the backend");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn client_ids_are_stable_and_distinct() {
        assert_eq!(client_id_of("gst_a"), client_id_of("gst_a"));
        assert_ne!(client_id_of("gst_a"), client_id_of("gst_b"));
        assert_ne!(client_id_of(""), client_id_of("gst_a"));
    }

    #[test]
    fn the_identity_path_ends_where_expected() {
        let p = identity_path();
        assert!(p.ends_with(format!("{DIR_NAME}/{FILE_NAME}")), "got {p:?}");
    }
}
