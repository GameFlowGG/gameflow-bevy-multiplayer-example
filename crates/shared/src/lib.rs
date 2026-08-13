//! Ghost Chase 1v1 simulation.
//!
//! Plain Rust: no Bevy, no networking, no async. The same code runs on the
//! authoritative server and inside the client's prediction loop, which is what
//! makes reconciliation a no-op in the common case.

pub mod difficulty;
pub mod ghosts;
pub mod maze;
pub mod movement;
pub mod pellets;
pub mod protocol;
pub mod score;
pub mod sim;

pub use difficulty::{Difficulty, GHOST_BASE_SPEED, RUNNER_SPEED};
pub use ghosts::{Ghost, GhostMode, RunnerTarget, Personality};
pub use maze::{chebyshev, Maze, Tile, MAZE, MAZE_H, MAZE_W};
pub use movement::{Dir, GridPos, Mover};
pub use pellets::{PelletField, PelletKind, Rng};
pub use protocol::{
    decode, encode, ClientMsg, GhostSnap, RunnerSnap, PelletDelta, ServerMsg, Snapshot, CH_EVENT,
    CH_INPUT, CH_SNAPSHOT, PROTOCOL_VERSION,
};
pub use sim::{MatchPhase, RunnerState, Runner, Sim, TickEvents};

/// Simulation rate. Fixed, and shared by server and client.
pub const TICK_HZ: u32 = 30;
pub const TICK_DT: f32 = 1.0 / TICK_HZ as f32;
