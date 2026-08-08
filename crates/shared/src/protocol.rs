//! The wire protocol.
//!
//! Deliberately small. A full snapshot is around 120 bytes, so there is no
//! delta compression and no interest management: every tick the server sends
//! the whole world. At 30Hz that is roughly 4 KB/s per client, which is not
//! worth optimising and is far easier to debug.

use glam::IVec2;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::ghosts::GhostMode;
use crate::movement::{Dir, GridPos};
use crate::pellets::PelletKind;
use crate::sim::PacState;

/// Inputs. Unreliable: a dropped input is superseded by the next one 33ms later.
pub const CH_INPUT: u8 = 0;
/// Snapshots. Unreliable for the same reason.
pub const CH_SNAPSHOT: u8 = 1;
/// Handshake and match end. Reliable and ordered: these must not be lost.
pub const CH_EVENT: u8 = 2;

/// Guards against a client connecting to a server built from other code.
pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// Sent once on connect. The server binds this connection to a roster slot
    /// only if the token matches what the backend issued.
    Hello {
        version: u16,
        player_id: String,
        session_token: String,
    },
    Input {
        seq: u32,
        dir: Dir,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Accepted. Carries everything needed to build the world locally.
    Welcome {
        slot: u8,
        seed: u64,
        /// The pellet field packed two bits per corridor tile.
        pellets: Vec<u8>,
        nicks: [String; 2],
    },
    Rejected {
        reason: String,
    },
    Snapshot(Snapshot),
    MatchOver {
        scores: [u32; 2],
        winner: Option<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PacSnap {
    pub pos: GridPos,
    pub state: PacState,
    pub lives: u8,
    pub score: u32,
    pub energized: bool,
    pub stunned: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GhostSnap {
    pub pos: GridPos,
    pub mode: GhostMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PelletDelta {
    Eaten(IVec2),
    Spawned(IVec2, PelletKind),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub tick: u32,
    pub elapsed_ms: u32,
    /// The most recent input sequence this client's slot had applied. Anything
    /// after it is still pending and gets replayed during reconciliation.
    pub last_processed_seq: u32,
    pub pacmen: [PacSnap; 2],
    pub ghosts: Vec<GhostSnap>,
    pub pellet_deltas: Vec<PelletDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError(&'static str);

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "protocol error: {}", self.0)
    }
}

impl std::error::Error for ProtocolError {}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, ProtocolError> {
    postcard::to_allocvec(value).map_err(|_| ProtocolError("encode failed"))
}

pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ProtocolError> {
    postcard::from_bytes(bytes).map_err(|_| ProtocolError("decode failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> Snapshot {
        let pac = PacSnap {
            pos: GridPos::new(IVec2::new(14, 23), Dir::Left),
            state: PacState::Alive,
            lives: 3,
            score: 12_340,
            energized: true,
            stunned: false,
        };
        Snapshot {
            tick: 5_400,
            elapsed_ms: 180_000,
            last_processed_seq: 5_399,
            pacmen: [pac.clone(), pac],
            ghosts: (0..7)
                .map(|i| GhostSnap {
                    pos: GridPos::new(IVec2::new(i, 14), Dir::Up),
                    mode: GhostMode::Chase,
                })
                .collect(),
            pellet_deltas: vec![
                PelletDelta::Eaten(IVec2::new(1, 1)),
                PelletDelta::Spawned(IVec2::new(2, 2), PelletKind::Normal),
                PelletDelta::Spawned(IVec2::new(3, 3), PelletKind::Power),
            ],
        }
    }

    #[test]
    fn a_snapshot_round_trips() {
        let snap = sample_snapshot();
        let bytes = encode(&ServerMsg::Snapshot(snap.clone())).unwrap();
        match decode::<ServerMsg>(&bytes).unwrap() {
            ServerMsg::Snapshot(got) => assert_eq!(got, snap),
            other => panic!("decoded the wrong variant: {other:?}"),
        }
    }

    /// The design leans on snapshots being cheap. If this ever fails, the
    /// no-delta-compression decision needs revisiting.
    #[test]
    fn a_full_snapshot_stays_small() {
        let bytes = encode(&ServerMsg::Snapshot(sample_snapshot())).unwrap();
        assert!(
            bytes.len() < 400,
            "snapshot grew to {} bytes, which breaks the bandwidth assumption",
            bytes.len()
        );
    }

    #[test]
    fn an_input_is_tiny() {
        let bytes = encode(&ClientMsg::Input {
            seq: 100_000,
            dir: Dir::Left,
        })
        .unwrap();
        assert!(bytes.len() <= 8, "input grew to {} bytes", bytes.len());
    }

    #[test]
    fn client_messages_round_trip() {
        let hello = ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            player_id: "gst_7f3a".into(),
            session_token: "tok".into(),
        };
        assert_eq!(decode::<ClientMsg>(&encode(&hello).unwrap()).unwrap(), hello);
    }

    #[test]
    fn match_over_round_trips_including_a_draw() {
        for winner in [Some(0u8), Some(1), None] {
            let msg = ServerMsg::MatchOver {
                scores: [1200, 1200],
                winner,
            };
            assert_eq!(decode::<ServerMsg>(&encode(&msg).unwrap()).unwrap(), msg);
        }
    }

    #[test]
    fn garbage_is_rejected_instead_of_panicking() {
        assert!(decode::<ClientMsg>(&[]).is_err());
        assert!(decode::<Snapshot>(&[0x01]).is_err());
    }

    #[test]
    fn the_pellet_sync_fits_in_a_welcome() {
        let pellets = crate::pellets::PelletField::new_full().snapshot_bits();
        let msg = ServerMsg::Welcome {
            slot: 0,
            seed: 42,
            pellets,
            nicks: ["yurei".into(), "rival".into()],
        };
        let bytes = encode(&msg).unwrap();
        assert!(bytes.len() < 200, "welcome grew to {} bytes", bytes.len());
    }
}
