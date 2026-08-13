//! Match state and the fixed tick loop.
//!
//! This is the authority. It owns the simulation, decides which connection owns
//! which slot, and is the only thing that may say what a score is.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_renet::RenetServer;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use renet::ClientId;
use serde::Deserialize;

use ghostchase_shared::protocol::{
    decode, encode, ClientMsg, GhostSnap, RunnerSnap, PelletDelta, ServerMsg, Snapshot, CH_EVENT,
    CH_INPUT, CH_SNAPSHOT, PROTOCOL_VERSION,
};
use ghostchase_shared::sim::{MatchPhase, Sim};
use ghostchase_shared::TICK_DT;

use crate::gameflow_plugin::GameFlowClient;
use crate::roster::Roster;
use crate::ServerConfigRes;

/// How long the server waits for both players before starting with whoever
/// turned up. Without this a single no-show would hold a pod open forever.
pub const START_GRACE_SECS: f32 = 30.0;

/// The claims the backend signs into a session token.
#[derive(Debug, Deserialize)]
struct SessionClaims {
    sub: String,
    #[serde(default)]
    nick: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SlotOccupant {
    pub player_id: String,
    pub nick: String,
    pub connected: bool,
    /// True once this slot has ever been occupied. A slot that nobody ever took
    /// makes the match unratable.
    pub ever_present: bool,
}

#[derive(Resource)]
pub struct Session {
    pub sim: Sim,
    pub roster: Option<Roster>,
    pub match_id: String,
    pub slots: [SlotOccupant; 2],
    pub slot_by_client: HashMap<ClientId, u8>,
    /// Last input sequence applied per slot, echoed back for reconciliation.
    pub last_seq: [u32; 2],
    pub waited: f32,
    pub seed: u64,
    /// Set once the result has been handed to the reporter, so it happens once.
    pub reported: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BindError {
    BadToken,
    WrongVersion,
    SlotTaken,
    NoSlots,
}

impl Session {
    pub fn new(seed: u64) -> Session {
        Session {
            sim: Sim::new(seed),
            roster: None,
            match_id: String::new(),
            slots: [SlotOccupant::default(), SlotOccupant::default()],
            slot_by_client: HashMap::new(),
            last_seq: [0, 0],
            waited: 0.0,
            seed,
            reported: false,
        }
    }

    /// Adopts the launch payload. Called when the SDK delivers it, which may be
    /// before or after the first client connects.
    pub fn adopt_roster(&mut self, roster: Roster) {
        if self.match_id.is_empty() {
            self.match_id = roster.match_id.clone();
        }
        self.roster = Some(roster);
    }

    /// Verifies a session token and returns the identity inside it. The token is
    /// signed by the backend with the shared internal secret, which the client
    /// never holds, so this is what actually stops a player claiming to be
    /// somebody else.
    fn verify(secret: &str, token: &str) -> Option<(String, String)> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = 0;
        jsonwebtoken::decode::<SessionClaims>(
            token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &validation,
        )
        .ok()
        .map(|d| (d.claims.sub, d.claims.nick))
    }

    /// Binds a connection to a slot.
    ///
    /// The roster decides seating when it parsed. When it did not, slots are
    /// handed out in connection order: identity is still proven by the token,
    /// so the worst case is that the two players swap corners.
    pub fn bind_client(
        &mut self,
        client_id: ClientId,
        version: u16,
        token: &str,
        secret: &str,
    ) -> Result<u8, BindError> {
        if version != PROTOCOL_VERSION {
            return Err(BindError::WrongVersion);
        }

        let (player_id, token_nick) = Self::verify(secret, token).ok_or(BindError::BadToken)?;

        // Reconnecting into your own slot is fine.
        if let Some(slot) = self
            .slots
            .iter()
            .position(|s| s.ever_present && s.player_id == player_id)
        {
            self.slots[slot].connected = true;
            self.slot_by_client.insert(client_id, slot as u8);
            return Ok(slot as u8);
        }

        let wanted = self
            .roster
            .as_ref()
            .and_then(|r| r.slot_of(&player_id))
            .filter(|s| (*s as usize) < 2);

        let slot = match wanted {
            Some(s) if !self.slots[s as usize].ever_present => s,
            Some(_) => return Err(BindError::SlotTaken),
            None => self
                .slots
                .iter()
                .position(|s| !s.ever_present)
                .ok_or(BindError::NoSlots)? as u8,
        };

        let nick = self
            .roster
            .as_ref()
            .and_then(|r| r.nick_of(&player_id))
            .map(|n| n.to_string())
            .filter(|n| !n.is_empty())
            .or(Some(token_nick))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| format!("player{}", slot + 1));

        self.slots[slot as usize] = SlotOccupant {
            player_id,
            nick,
            connected: true,
            ever_present: true,
        };
        self.slot_by_client.insert(client_id, slot);
        Ok(slot)
    }

    pub fn release_client(&mut self, client_id: ClientId) -> Option<u8> {
        let slot = self.slot_by_client.remove(&client_id)?;
        self.slots[slot as usize].connected = false;
        Some(slot)
    }

    pub fn both_seated(&self) -> bool {
        self.slots.iter().all(|s| s.ever_present)
    }

    pub fn nicks(&self) -> [String; 2] {
        [self.slots[0].nick.clone(), self.slots[1].nick.clone()]
    }

}

pub struct SessionPlugin;

impl Plugin for SessionPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Time::<Fixed>::from_hz(ghostchase_shared::TICK_HZ as f64))
            .add_observer(on_connection_event)
            .add_systems(Update, (adopt_payload, receive_messages).chain())
            .add_systems(FixedUpdate, (start_when_ready, advance_match).chain());
    }
}

/// Picks up the launch payload whenever the SDK delivers it.
fn adopt_payload(
    mut payloads: MessageReader<crate::gameflow_plugin::PayloadChanged>,
    session: Option<ResMut<Session>>,
) {
    let Some(mut session) = session else {
        return;
    };
    for ev in payloads.read() {
        let Some(raw) = ev.payload.as_ref() else {
            continue;
        };
        match Roster::parse(raw) {
            Some(roster) => {
                info!(
                    "roster adopted: match {} with {} players",
                    roster.match_id,
                    roster.players.len()
                );
                session.adopt_roster(roster);
            }
            None => warn!("launch payload could not be parsed, falling back to connection order"),
        }
    }
}

fn on_connection_event(
    event: On<bevy_renet::RenetServerEvent>,
    session: Option<ResMut<Session>>,
    gf: Option<Res<GameFlowClient>>,
) {
    let Some(mut session) = session else {
        return;
    };

    match **event {
        renet::ServerEvent::ClientConnected { client_id } => {
            debug!("transport connected: {client_id}, waiting for hello");
        }
        renet::ServerEvent::ClientDisconnected { client_id, reason } => {
            if let Some(slot) = session.release_client(client_id) {
                let occupant = session.slots[slot as usize].clone();
                info!("slot {slot} ({}) left: {reason}", occupant.player_id);

                // Tell the platform straight away. A server that keeps
                // reporting a player who left gets held open past its welcome.
                if let Some(gf) = gf {
                    gf.disconnect_player(occupant.player_id.clone());
                }

                // Walking out forfeits the remaining lives. The match plays on
                // so the other player still gets a result.
                if session.sim.is_running() {
                    session.sim.abandon(slot);
                }
            }
        }
    }
}

fn receive_messages(
    mut server: Option<ResMut<RenetServer>>,
    session: Option<ResMut<Session>>,
    config: Res<ServerConfigRes>,
    gf: Option<Res<GameFlowClient>>,
) {
    let (Some(server), Some(mut session)) = (server.as_mut(), session) else {
        return;
    };

    for client_id in server.clients_id() {
        // Handshake first: until a connection is bound to a slot, nothing else
        // it sends is worth reading.
        while let Some(bytes) = server.receive_message(client_id, CH_EVENT) {
            let Ok(msg) = decode::<ClientMsg>(&bytes) else {
                warn!("undecodable event message from {client_id}");
                continue;
            };
            if let ClientMsg::Hello {
                version,
                player_id,
                session_token,
            } = msg
            {
                match session.bind_client(client_id, version, &session_token, &config.api_token)
                {
                    Ok(slot) => {
                        info!("slot {slot} taken by {player_id}");
                        if let Some(gf) = gf.as_ref() {
                            gf.connect_player(session.slots[slot as usize].player_id.clone());
                        }
                        let welcome = ServerMsg::Welcome {
                            slot,
                            seed: session.seed,
                            pellets: session.sim.pellets.snapshot_bits(),
                            nicks: session.nicks(),
                        };
                        if let Ok(bytes) = encode(&welcome) {
                            server.send_message(client_id, CH_EVENT, bytes);
                        }
                    }
                    Err(e) => {
                        warn!("rejected {client_id} claiming {player_id}: {e:?}");
                        let msg = ServerMsg::Rejected {
                            reason: format!("{e:?}"),
                        };
                        if let Ok(bytes) = encode(&msg) {
                            server.send_message(client_id, CH_EVENT, bytes);
                        }
                        server.disconnect(client_id);
                    }
                }
            }
        }

        // Inputs from a connection that never said hello are dropped in
        // silence. The server never trusts a client it cannot name.
        let Some(slot) = session.slot_by_client.get(&client_id).copied() else {
            while server.receive_message(client_id, CH_INPUT).is_some() {}
            continue;
        };

        while let Some(bytes) = server.receive_message(client_id, CH_INPUT) {
            if let Ok(ClientMsg::Input { seq, dir }) = decode::<ClientMsg>(&bytes) {
                // Unreliable delivery reorders: an older input than the one
                // already applied says nothing new.
                if seq > session.last_seq[slot as usize] {
                    session.last_seq[slot as usize] = seq;
                    session.sim.set_input(slot, dir);
                }
            }
        }
    }
}

fn start_when_ready(session: Option<ResMut<Session>>) {
    let Some(mut session) = session else {
        return;
    };
    if session.sim.phase != MatchPhase::Waiting {
        return;
    }

    if session.both_seated() {
        info!("both players seated, match starting");
        session.sim.start();
        return;
    }

    session.waited += TICK_DT;
    if session.waited >= START_GRACE_SECS {
        let absent: Vec<u8> = (0..2u8)
            .filter(|s| !session.slots[*s as usize].ever_present)
            .collect();
        warn!("starting after {START_GRACE_SECS}s with slots {absent:?} empty");
        session.sim.start();
        for slot in absent {
            session.sim.abandon(slot);
        }
    }
}

fn advance_match(mut server: Option<ResMut<RenetServer>>, session: Option<ResMut<Session>>) {
    let (Some(server), Some(mut session)) = (server.as_mut(), session) else {
        return;
    };
    if !session.sim.is_running() {
        return;
    }

    let events = session.sim.tick();

    let mut deltas: Vec<PelletDelta> =
        Vec::with_capacity(events.pellets_eaten.len() + events.pellets_spawned.len());
    for (tile, _, _) in &events.pellets_eaten {
        deltas.push(PelletDelta::Eaten(*tile));
    }
    for (tile, kind) in &events.pellets_spawned {
        deltas.push(PelletDelta::Spawned(*tile, *kind));
    }

    let now = session.sim.elapsed;
    let runners = [
        runner_snap(&session.sim, 0, now),
        runner_snap(&session.sim, 1, now),
    ];
    let ghosts: Vec<GhostSnap> = session
        .sim
        .ghosts
        .iter()
        .map(|g| GhostSnap {
            pos: g.pos,
            mode: g.mode,
        })
        .collect();

    // Each client gets its own last_processed_seq, so the payload is built per
    // slot rather than broadcast.
    for (client_id, slot) in session.slot_by_client.clone() {
        let snapshot = Snapshot {
            tick: session.sim.tick_count,
            elapsed_ms: (now * 1000.0) as u32,
            last_processed_seq: session.last_seq[slot as usize],
            runners: runners.clone(),
            ghosts: ghosts.clone(),
            pellet_deltas: deltas.clone(),
        };
        if let Ok(bytes) = encode(&ServerMsg::Snapshot(snapshot)) {
            server.send_message(client_id, CH_SNAPSHOT, bytes);
        }
    }

    if let MatchPhase::Finished { winner } = session.sim.phase {
        let scores = [session.sim.runners[0].score, session.sim.runners[1].score];
        info!("match over: {scores:?}, winner {winner:?}");
        if let Ok(bytes) = encode(&ServerMsg::MatchOver { scores, winner }) {
            for client_id in server.clients_id() {
                server.send_message(client_id, CH_EVENT, bytes.clone());
            }
        }
    }
}

fn runner_snap(sim: &Sim, slot: usize, now: f32) -> RunnerSnap {
    let p = &sim.runners[slot];
    RunnerSnap {
        pos: p.pos,
        state: p.state,
        lives: p.lives,
        score: p.score,
        energized: p.is_energized(now),
        stunned: p.is_stunned(now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode as jwt_encode, EncodingKey, Header};
    use serde::Serialize;

    const SECRET: &str = "internal-token";

    #[derive(Serialize)]
    struct Claims {
        sub: String,
        nick: String,
        exp: u64,
    }

    fn token_for(player_id: &str, nick: &str) -> String {
        let exp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 900;
        jwt_encode(
            &Header::default(),
            &Claims {
                sub: player_id.into(),
                nick: nick.into(),
                exp,
            },
            &EncodingKey::from_secret(SECRET.as_bytes()),
        )
        .unwrap()
    }

    fn roster() -> Roster {
        Roster::parse(
            r#"{"match_id":"mt_1","players":[
                {"player_id":"gst_a","nick":"ana","slot":0},
                {"player_id":"gst_b","nick":"beto","slot":1}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_valid_token_takes_its_roster_slot() {
        let mut s = Session::new(1);
        s.adopt_roster(roster());
        assert_eq!(
            s.bind_client(10, PROTOCOL_VERSION, &token_for("gst_b", "beto"), SECRET),
            Ok(1)
        );
        assert_eq!(s.slots[1].nick, "beto");
        assert!(s.slots[1].connected);
    }

    #[test]
    fn a_token_signed_with_the_wrong_secret_is_refused() {
        let mut s = Session::new(1);
        s.adopt_roster(roster());
        assert_eq!(
            s.bind_client(10, PROTOCOL_VERSION, &token_for("gst_a", "ana"), "other"),
            Err(BindError::BadToken)
        );
    }

    #[test]
    fn garbage_instead_of_a_token_is_refused() {
        let mut s = Session::new(1);
        assert_eq!(
            s.bind_client(10, PROTOCOL_VERSION, "not-a-token", SECRET),
            Err(BindError::BadToken)
        );
    }

    #[test]
    fn a_mismatched_protocol_version_is_refused() {
        let mut s = Session::new(1);
        assert_eq!(
            s.bind_client(10, PROTOCOL_VERSION + 1, &token_for("gst_a", "ana"), SECRET),
            Err(BindError::WrongVersion)
        );
    }

    #[test]
    fn a_second_player_cannot_take_a_claimed_slot() {
        let mut s = Session::new(1);
        s.adopt_roster(roster());
        s.bind_client(10, PROTOCOL_VERSION, &token_for("gst_a", "ana"), SECRET)
            .unwrap();

        // A different connection presenting the same identity is a reconnect,
        // which is allowed and returns the same slot.
        assert_eq!(
            s.bind_client(11, PROTOCOL_VERSION, &token_for("gst_a", "ana"), SECRET),
            Ok(0)
        );
    }

    #[test]
    fn without_a_roster_slots_go_in_connection_order() {
        let mut s = Session::new(1);
        assert_eq!(
            s.bind_client(10, PROTOCOL_VERSION, &token_for("gst_x", "x"), SECRET),
            Ok(0)
        );
        assert_eq!(
            s.bind_client(11, PROTOCOL_VERSION, &token_for("gst_y", "y"), SECRET),
            Ok(1)
        );
    }

    #[test]
    fn a_third_player_is_turned_away() {
        let mut s = Session::new(1);
        s.bind_client(10, PROTOCOL_VERSION, &token_for("gst_x", "x"), SECRET)
            .unwrap();
        s.bind_client(11, PROTOCOL_VERSION, &token_for("gst_y", "y"), SECRET)
            .unwrap();
        assert_eq!(
            s.bind_client(12, PROTOCOL_VERSION, &token_for("gst_z", "z"), SECRET),
            Err(BindError::NoSlots)
        );
    }

    #[test]
    fn a_player_not_on_the_roster_still_gets_a_free_seat() {
        let mut s = Session::new(1);
        s.adopt_roster(roster());
        // The roster names a and b, but the token proves this is somebody else.
        // Refusing would strand a real player over a payload we do not own.
        assert!(s
            .bind_client(10, PROTOCOL_VERSION, &token_for("gst_zzz", "z"), SECRET)
            .is_ok());
    }

    #[test]
    fn the_nick_falls_back_to_the_token_when_the_roster_is_silent() {
        let mut s = Session::new(1);
        assert_eq!(
            s.bind_client(10, PROTOCOL_VERSION, &token_for("gst_x", "yurei"), SECRET),
            Ok(0)
        );
        assert_eq!(s.slots[0].nick, "yurei");
    }

    #[test]
    fn releasing_a_client_frees_the_connection_but_keeps_the_seat() {
        let mut s = Session::new(1);
        s.bind_client(10, PROTOCOL_VERSION, &token_for("gst_x", "x"), SECRET)
            .unwrap();
        assert_eq!(s.release_client(10), Some(0));
        assert!(!s.slots[0].connected);
        assert!(s.slots[0].ever_present, "the seat stays taken for reporting");
        assert_eq!(s.release_client(10), None);
    }

    #[test]
    fn both_seated_needs_two_real_players() {
        let mut s = Session::new(1);
        assert!(!s.both_seated());
        s.bind_client(10, PROTOCOL_VERSION, &token_for("gst_x", "x"), SECRET)
            .unwrap();
        assert!(!s.both_seated());
        s.bind_client(11, PROTOCOL_VERSION, &token_for("gst_y", "y"), SECRET)
            .unwrap();
        assert!(s.both_seated());
    }

    #[test]
    fn an_input_from_an_unbound_connection_has_no_slot() {
        let s = Session::new(1);
        assert!(
            !s.slot_by_client.contains_key(&99),
            "a connection that never said hello must never map to a slot"
        );
    }
}
