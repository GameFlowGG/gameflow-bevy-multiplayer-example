//! Talking to the game server.
//!
//! One message goes up: the direction the player wants. Everything else comes
//! down. The client owns no authority at all, which is why there is no code
//! here that could ever report a score.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::SystemTime;

use bevy::prelude::*;
use bevy_renet::netcode::{ClientAuthentication, NetcodeClientPlugin, NetcodeClientTransport};
use bevy_renet::{RenetClient, RenetClientPlugin};
use renet::{ChannelConfig, ConnectionConfig, SendType};

use ghostchase_shared::ghosts::GhostMode;
use ghostchase_shared::maze::{SPAWN_P0, SPAWN_P1};
use ghostchase_shared::movement::{Dir, GridPos};
use ghostchase_shared::pellets::PelletField;
use ghostchase_shared::protocol::{
    decode, encode, ClientMsg, PelletDelta, ServerMsg, CH_EVENT, CH_INPUT, CH_SNAPSHOT,
    PROTOCOL_VERSION,
};
use ghostchase_shared::sim::RunnerState;

use crate::identity::client_id_of;
use crate::predict::{Interp, Local};
use crate::Screen;

const PROTOCOL_ID: u64 = 0x5041_434D_414E_0001;
const RESEND: std::time::Duration = std::time::Duration::from_millis(200);

/// Must match the server exactly. A mismatch here does not error, it just
/// silently drops messages, which is the worst kind of bug to chase.
fn connection_config() -> ConnectionConfig {
    let channels = vec![
        ChannelConfig {
            channel_id: CH_INPUT,
            max_memory_usage_bytes: 64 * 1024,
            send_type: SendType::Unreliable,
        },
        ChannelConfig {
            channel_id: CH_SNAPSHOT,
            max_memory_usage_bytes: 256 * 1024,
            send_type: SendType::Unreliable,
        },
        ChannelConfig {
            channel_id: CH_EVENT,
            max_memory_usage_bytes: 256 * 1024,
            send_type: SendType::ReliableOrdered { resend_time: RESEND },
        },
    ];
    ConnectionConfig {
        available_bytes_per_tick: 60_000,
        server_channels_config: channels.clone(),
        client_channels_config: channels,
    }
}

/// Everything the client knows about the match in progress.
#[derive(Resource)]
pub struct Match {
    pub slot: u8,
    pub local: Local,
    /// The rival, interpolated. There is no prediction for them: guessing at
    /// somebody else's input buys nothing and looks worse when it is wrong.
    pub remote: Interp,
    pub remote_pos: GridPos,
    pub ghosts: Vec<(Interp, GhostMode, GridPos)>,
    pub pellets: PelletField,
    pub scores: [u32; 2],
    pub lives: [u8; 2],
    pub states: [RunnerState; 2],
    pub energized: [bool; 2],
    pub stunned: [bool; 2],
    pub nicks: [String; 2],
    pub elapsed_ms: u32,
    /// Set once the server says the match is over.
    pub result: Option<MatchResult>,
}

#[derive(Debug, Clone, Copy)]
pub struct MatchResult {
    pub scores: [u32; 2],
    pub winner: Option<u8>,
}

impl Match {
    fn new(slot: u8, pellets: PelletField, nicks: [String; 2]) -> Match {
        let local_spawn = if slot == 0 { SPAWN_P0 } else { SPAWN_P1 };
        let remote_spawn = if slot == 0 { SPAWN_P1 } else { SPAWN_P0 };
        let local_dir = if slot == 0 { Dir::Right } else { Dir::Left };

        Match {
            slot,
            local: Local::new(GridPos::new(local_spawn, local_dir)),
            remote: Interp::new(Vec2::new(remote_spawn.x as f32, remote_spawn.y as f32)),
            remote_pos: GridPos::new(remote_spawn, Dir::Left),
            ghosts: Vec::new(),
            pellets,
            scores: [0, 0],
            lives: [3, 3],
            states: [RunnerState::Alive, RunnerState::Alive],
            energized: [false, false],
            stunned: [false, false],
            nicks,
            elapsed_ms: 0,
            result: None,
        }
    }

    pub fn rival_slot(&self) -> usize {
        1 - self.slot as usize
    }
}

/// Where the client was told to connect, plus the token proving who it is.
#[derive(Resource, Debug, Clone)]
pub struct Assignment {
    pub connection: String,
    pub session_token: String,
    pub player_id: String,
}

/// The direction the player is currently asking for.
#[derive(Resource, Default, Debug)]
pub struct Desired(pub Option<Dir>);

/// Whether the one-time `Hello` has been sent for the current connection.
///
/// Without this the client re-sends `Hello` every frame until the `Welcome`
/// lands, and the server mints a fresh `Welcome` for each one. Those extra
/// welcomes used to re-enter `Playing` and spawn a second runner, which then
/// froze the board because the placement query expects exactly one.
#[derive(Resource, Default)]
struct Handshake {
    hello_sent: bool,
}

pub struct NetClientPlugin;

impl Plugin for NetClientPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((RenetClientPlugin, NetcodeClientPlugin))
            .init_resource::<Desired>()
            .init_resource::<Handshake>()
            .add_systems(Update, (read_keyboard, receive).chain())
            .add_systems(FixedUpdate, send_input);
    }
}

/// Opens the socket and inserts the transport. Errors are returned rather than
/// logged and swallowed, so the UI can tell the player what went wrong.
pub fn connect(commands: &mut Commands, assignment: &Assignment) -> Result<(), String> {
    let server_addr: SocketAddr = assignment
        .connection
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {}: {e}", assignment.connection))?
        .next()
        .ok_or_else(|| format!("{} resolved to nothing", assignment.connection))?;

    // Port 0 lets the OS pick; the client does not need a known port.
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("could not open a socket: {e}"))?;

    let authentication = ClientAuthentication::Unsecure {
        protocol_id: PROTOCOL_ID,
        client_id: client_id_of(&assignment.player_id),
        server_addr,
        user_data: None,
    };

    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|e| format!("system clock is before the unix epoch: {e}"))?;

    let transport = NetcodeClientTransport::new(current_time, authentication, socket)
        .map_err(|e| format!("could not start the transport: {e}"))?;

    commands.insert_resource(RenetClient::new(connection_config()));
    commands.insert_resource(transport);
    // A new transport is a new handshake: allow exactly one Hello for it.
    commands.insert_resource(Handshake::default());
    info!("connecting to {server_addr}");
    Ok(())
}

pub fn disconnect(commands: &mut Commands) {
    commands.remove_resource::<RenetClient>();
    commands.remove_resource::<NetcodeClientTransport>();
    commands.remove_resource::<Match>();
    // Clear the assignment too: a leftover one makes the reply handler think it
    // is already connecting, so the next queue never dials the new server.
    commands.remove_resource::<Assignment>();
    // Drop the last direction so the next match does not start walking on its own.
    commands.insert_resource(Desired::default());
    commands.insert_resource(Handshake::default());
}

fn read_keyboard(keys: Res<ButtonInput<KeyCode>>, mut desired: ResMut<Desired>) {
    // Last key pressed wins, so a quick double tap does what it looks like.
    for (code, dir) in [
        (KeyCode::ArrowUp, Dir::Up),
        (KeyCode::KeyW, Dir::Up),
        (KeyCode::ArrowDown, Dir::Down),
        (KeyCode::KeyS, Dir::Down),
        (KeyCode::ArrowLeft, Dir::Left),
        (KeyCode::KeyA, Dir::Left),
        (KeyCode::ArrowRight, Dir::Right),
        (KeyCode::KeyD, Dir::Right),
    ] {
        if keys.just_pressed(code) {
            desired.0 = Some(dir);
        }
    }
}

/// One input per simulation tick, predicted locally and sent unreliably.
fn send_input(
    client: Option<ResMut<RenetClient>>,
    game: Option<ResMut<Match>>,
    desired: Res<Desired>,
) {
    let (Some(mut client), Some(mut game)) = (client, game) else {
        return;
    };
    let Some(dir) = desired.0 else {
        return;
    };
    if game.result.is_some() {
        return;
    }
    // A stunned or dead runner does not move, and predicting otherwise would
    // guarantee a correction on the next snapshot.
    let slot = game.slot as usize;
    if game.stunned[slot] || game.states[slot] != RunnerState::Alive {
        return;
    }

    let seq = game.local.push_and_apply(dir);
    if let Ok(bytes) = encode(&ClientMsg::Input { seq, dir }) {
        client.send_message(CH_INPUT, bytes);
    }
}

fn receive(
    mut commands: Commands,
    client: Option<ResMut<RenetClient>>,
    mut game: Option<ResMut<Match>>,
    assignment: Option<Res<Assignment>>,
    mut handshake: ResMut<Handshake>,
    mut next: ResMut<NextState<Screen>>,
) {
    let (Some(mut client), Some(assignment)) = (client, assignment) else {
        return;
    };

    // Say hello exactly once per connection. Sending it every frame until the
    // Welcome arrives makes the server mint a Welcome per Hello, and each extra
    // Welcome would otherwise re-seat us and duplicate the board.
    if client.is_connected() && !handshake.hello_sent {
        let hello = ClientMsg::Hello {
            version: PROTOCOL_VERSION,
            player_id: assignment.player_id.clone(),
            session_token: assignment.session_token.clone(),
        };
        if let Ok(bytes) = encode(&hello) {
            client.send_message(CH_EVENT, bytes);
        }
        handshake.hello_sent = true;
    }

    while let Some(bytes) = client.receive_message(CH_EVENT) {
        match decode::<ServerMsg>(&bytes) {
            Ok(ServerMsg::Welcome {
                slot,
                pellets,
                nicks,
                ..
            }) => {
                // A duplicate Welcome (from a resent Hello, say) must not reset
                // the match or respawn the board on top of the live one.
                if game.is_some() {
                    continue;
                }
                info!("seated in slot {slot}");
                let field = PelletField::from_snapshot_bits(&pellets);
                let fresh = Match::new(slot, field, nicks);
                commands.insert_resource(fresh);
                next.set(Screen::Playing);
                // The rest of this frame has no Match to write into yet.
                return;
            }
            Ok(ServerMsg::Rejected { reason }) => {
                error!("the server refused the connection: {reason}");
                next.set(Screen::Menu);
                return;
            }
            Ok(ServerMsg::MatchOver { scores, winner }) => {
                if let Some(game) = game.as_mut() {
                    game.result = Some(MatchResult { scores, winner });
                }
                next.set(Screen::Result);
            }
            _ => {}
        }
    }

    let Some(game) = game.as_mut() else {
        // Drain snapshots that arrived before the welcome rather than letting
        // them queue up and apply out of order later.
        while client.receive_message(CH_SNAPSHOT).is_some() {}
        return;
    };

    // Only the newest snapshot matters. Unreliable delivery can reorder, and
    // applying a stale world after a fresh one would visibly rewind everything.
    let mut newest: Option<ghostchase_shared::protocol::Snapshot> = None;
    while let Some(bytes) = client.receive_message(CH_SNAPSHOT) {
        if let Ok(ServerMsg::Snapshot(snap)) = decode::<ServerMsg>(&bytes) {
            let keep = newest.as_ref().map(|n| snap.tick > n.tick).unwrap_or(true);
            if keep {
                newest = Some(snap);
            }
        }
    }

    let Some(snap) = newest else {
        return;
    };
    apply_snapshot(game, snap);
}

fn apply_snapshot(game: &mut Match, snap: ghostchase_shared::protocol::Snapshot) {
    let slot = game.slot as usize;
    let rival = game.rival_slot();

    game.elapsed_ms = snap.elapsed_ms;
    for i in 0..2 {
        game.scores[i] = snap.runners[i].score;
        game.lives[i] = snap.runners[i].lives;
        game.states[i] = snap.runners[i].state;
        game.energized[i] = snap.runners[i].energized;
        game.stunned[i] = snap.runners[i].stunned;
    }

    // Anything the client could not have predicted, such as dying or being
    // stunned, means the prediction is worthless: take the server's word.
    let authoritative = snap.runners[slot].pos;
    if snap.runners[slot].state != RunnerState::Alive || snap.runners[slot].stunned {
        game.local.force(authoritative);
    } else {
        game.local.reconcile(snap.last_processed_seq, authoritative);
    }

    game.remote_pos = snap.runners[rival].pos;
    let world = snap.runners[rival].pos.world();
    game.remote.push(Vec2::new(world.x, world.y));

    while game.ghosts.len() < snap.ghosts.len() {
        let g = &snap.ghosts[game.ghosts.len()];
        let w = g.pos.world();
        game.ghosts
            .push((Interp::new(Vec2::new(w.x, w.y)), g.mode, g.pos));
    }
    for (i, g) in snap.ghosts.iter().enumerate() {
        let w = g.pos.world();
        game.ghosts[i].0.push(Vec2::new(w.x, w.y));
        game.ghosts[i].1 = g.mode;
        game.ghosts[i].2 = g.pos;
    }

    for delta in snap.pellet_deltas {
        match delta {
            PelletDelta::Eaten(tile) => {
                game.pellets.take(tile);
            }
            PelletDelta::Spawned(tile, kind) => game.pellets.put(tile, kind),
        }
    }
}

/// Advances every interpolator. Runs on render frames, not simulation ticks,
/// which is the point: motion stays smooth above 30Hz.
pub fn advance_interpolation(time: Res<Time>, game: Option<ResMut<Match>>) {
    let Some(mut game) = game else {
        return;
    };
    let dt = time.delta_secs();
    game.remote.advance(dt);
    for (interp, _, _) in game.ghosts.iter_mut() {
        interp.advance(dt);
    }
}

