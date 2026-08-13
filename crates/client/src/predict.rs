//! Client side prediction, reconciliation and interpolation.
//!
//! Only the local runner is predicted. Ghosts, pellets and the rival arrive as
//! authoritative state and are interpolated between snapshots. That asymmetry
//! is the whole reason this file is short: the client never has to reproduce
//! ghost AI or the pellet drip, so there is no shared RNG to keep in step and
//! no way for the two sides to diverge except through the one thing the player
//! actually controls.
//!
//! Because grid movement is exact, replaying an input produces bit-identical
//! results on both ends. In the normal case reconciliation moves nothing.

use std::collections::VecDeque;

use bevy::math::Vec2;
use ghostchase_shared::difficulty::RUNNER_SPEED;
use ghostchase_shared::movement::{Dir, GridPos, Mover};
use ghostchase_shared::TICK_DT;

/// The local player's predicted state.
#[derive(Debug, Clone)]
pub struct Local {
    pub pos: GridPos,
    /// Inputs sent but not yet confirmed by the server, oldest first.
    pub pending: VecDeque<(u32, Dir)>,
    pub next_seq: u32,
    /// How far the last correction moved us, in tiles. Useful to watch when
    /// something feels wrong: a healthy connection sits at zero.
    pub last_correction: f32,
}

impl Local {
    pub fn new(pos: GridPos) -> Local {
        Local {
            pos,
            pending: VecDeque::new(),
            next_seq: 1,
            last_correction: 0.0,
        }
    }

    /// Applies an input locally and records it for replay. Returns the sequence
    /// number to put on the wire.
    pub fn push_and_apply(&mut self, dir: Dir) -> u32 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.pending.push_back((seq, dir));
        self.pos
            .advance(Some(dir), Mover::Runner, RUNNER_SPEED, TICK_DT);
        seq
    }

    /// Snaps to the authoritative state and replays whatever the server has not
    /// seen yet.
    pub fn reconcile(&mut self, last_processed_seq: u32, authoritative: GridPos) {
        let before = self.pos.world();

        while let Some((seq, _)) = self.pending.front() {
            if *seq <= last_processed_seq {
                self.pending.pop_front();
            } else {
                break;
            }
        }

        self.pos = authoritative;
        for (_, dir) in self.pending.iter() {
            self.pos
                .advance(Some(*dir), Mover::Runner, RUNNER_SPEED, TICK_DT);
        }

        self.last_correction = (before - self.pos.world()).length();
    }

    /// Drops the prediction entirely and accepts the server's word. Used when
    /// the server did something the client could not have known about, such as
    /// a death or a stun.
    pub fn force(&mut self, authoritative: GridPos) {
        self.pending.clear();
        self.pos = authoritative;
        self.last_correction = 0.0;
    }
}

/// A remote entity's render position, moving between the last two snapshots.
///
/// Snapshots land every 33ms; drawing the newest one directly would step the
/// world at 30Hz on a 60Hz-plus display. Interpolating between the previous and
/// current sample costs one frame of latency on things the player does not
/// control, which nobody can feel, and removes the stutter, which everybody can.
#[derive(Debug, Clone, Copy)]
pub struct Interp {
    prev: Vec2,
    cur: Vec2,
    elapsed: f32,
}

/// One snapshot interval. Interpolation is spread across exactly this long.
pub const SNAPSHOT_INTERVAL: f32 = TICK_DT;
/// A jump larger than this is a tunnel wrap or a respawn, not movement, so it
/// is snapped instead of smeared across the screen.
const TELEPORT_TILES: f32 = 3.0;

impl Interp {
    pub fn new(at: Vec2) -> Interp {
        Interp {
            prev: at,
            cur: at,
            elapsed: SNAPSHOT_INTERVAL,
        }
    }

    /// Feeds a new authoritative sample.
    pub fn push(&mut self, next: Vec2) {
        let from = self.render();
        if (next - from).length() > TELEPORT_TILES {
            self.prev = next;
            self.cur = next;
            self.elapsed = SNAPSHOT_INTERVAL;
            return;
        }
        self.prev = from;
        self.cur = next;
        self.elapsed = 0.0;
    }

    pub fn advance(&mut self, dt: f32) {
        self.elapsed = (self.elapsed + dt).min(SNAPSHOT_INTERVAL);
    }

    pub fn render(&self) -> Vec2 {
        let t = (self.elapsed / SNAPSHOT_INTERVAL).clamp(0.0, 1.0);
        self.prev.lerp(self.cur, t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostchase_shared::maze::SPAWN_P0;

    fn start() -> GridPos {
        GridPos::new(SPAWN_P0, Dir::Right)
    }

    /// Replays the same inputs the way the server would.
    fn server_would(mut pos: GridPos, dir: Dir, steps: usize) -> GridPos {
        for _ in 0..steps {
            pos.advance(Some(dir), Mover::Runner, RUNNER_SPEED, TICK_DT);
        }
        pos
    }

    #[test]
    fn sequence_numbers_start_at_one_and_increase() {
        let mut local = Local::new(start());
        assert_eq!(local.push_and_apply(Dir::Right), 1);
        assert_eq!(local.push_and_apply(Dir::Right), 2);
        assert_eq!(local.pending.len(), 2);
    }

    #[test]
    fn a_fully_acknowledged_client_agrees_exactly_with_the_server() {
        let mut local = Local::new(start());
        for _ in 0..10 {
            local.push_and_apply(Dir::Right);
        }
        let authoritative = server_would(start(), Dir::Right, 10);

        local.reconcile(10, authoritative);

        assert!(local.pending.is_empty());
        assert_eq!(local.pos, authoritative);
        assert!(
            local.last_correction < 1e-5,
            "a correct prediction moved the player by {}",
            local.last_correction
        );
    }

    #[test]
    fn unacknowledged_inputs_are_replayed_and_kept() {
        let mut local = Local::new(start());
        for _ in 0..10 {
            local.push_and_apply(Dir::Right);
        }
        // The server has only seen the first six.
        let authoritative = server_would(start(), Dir::Right, 6);

        local.reconcile(6, authoritative);

        assert_eq!(local.pending.len(), 4, "the last four are still in flight");
        assert_eq!(
            local.pos,
            server_would(start(), Dir::Right, 10),
            "replaying the pending inputs must land where the client already was"
        );
        assert!(local.last_correction < 1e-5);
    }

    #[test]
    fn a_disagreement_shows_up_as_a_correction() {
        let mut local = Local::new(start());
        for _ in 0..10 {
            local.push_and_apply(Dir::Right);
        }
        // The server says we never moved: something rejected the inputs.
        local.reconcile(10, start());

        assert_eq!(local.pos, start());
        assert!(
            local.last_correction > 1.0,
            "a real disagreement should register, got {}",
            local.last_correction
        );
    }

    #[test]
    fn an_older_acknowledgement_does_not_drop_newer_inputs() {
        let mut local = Local::new(start());
        for _ in 0..5 {
            local.push_and_apply(Dir::Right);
        }
        local.reconcile(2, server_would(start(), Dir::Right, 2));
        assert_eq!(local.pending.len(), 3);

        // A late duplicate of an older snapshot must not resurrect anything.
        local.reconcile(2, server_would(start(), Dir::Right, 2));
        assert_eq!(local.pending.len(), 3);
    }

    #[test]
    fn forcing_clears_the_prediction() {
        let mut local = Local::new(start());
        for _ in 0..5 {
            local.push_and_apply(Dir::Right);
        }
        let elsewhere = GridPos::new(ghostchase_shared::maze::SPAWN_P1, Dir::Left);
        local.force(elsewhere);

        assert!(local.pending.is_empty());
        assert_eq!(local.pos, elsewhere);
        assert_eq!(local.last_correction, 0.0);
    }

    #[test]
    fn interpolation_walks_from_one_sample_to_the_next() {
        let mut interp = Interp::new(Vec2::ZERO);
        interp.push(Vec2::new(1.0, 0.0));

        assert!(interp.render().x < 0.01, "should start at the old sample");
        interp.advance(SNAPSHOT_INTERVAL / 2.0);
        let mid = interp.render().x;
        assert!((0.4..0.6).contains(&mid), "halfway was {mid}");

        interp.advance(SNAPSHOT_INTERVAL);
        assert!((interp.render().x - 1.0).abs() < 1e-5, "should settle exactly");
    }

    #[test]
    fn interpolation_never_overshoots() {
        let mut interp = Interp::new(Vec2::ZERO);
        interp.push(Vec2::new(1.0, 0.0));
        interp.advance(SNAPSHOT_INTERVAL * 100.0);
        assert!((interp.render().x - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_tunnel_wrap_snaps_instead_of_flying_across_the_maze() {
        let mut interp = Interp::new(Vec2::new(0.0, 14.0));
        interp.push(Vec2::new(27.0, 14.0));
        assert_eq!(
            interp.render(),
            Vec2::new(27.0, 14.0),
            "a wrap must teleport, not slide through every wall on the way"
        );
    }

    #[test]
    fn interpolation_is_continuous_across_back_to_back_samples() {
        let mut interp = Interp::new(Vec2::ZERO);
        interp.push(Vec2::new(1.0, 0.0));
        interp.advance(SNAPSHOT_INTERVAL / 2.0);
        let before = interp.render();

        // A new sample arriving early must not jump backwards.
        interp.push(Vec2::new(2.0, 0.0));
        let after = interp.render();
        assert!((before - after).length() < 1e-5, "{before:?} jumped to {after:?}");
    }
}
