//! Scoring and timing constants.

/// A plain pellet.
pub const PELLET: u32 = 10;
/// A power pellet.
pub const POWER_PELLET: u32 = 50;
/// Ghosts eaten within one energization, in order. Past the fourth the chain
/// stays at the last value.
pub const GHOST_CHAIN: [u32; 4] = [200, 400, 800, 1600];

/// Share of the victim's score taken when you eat your rival.
pub const STEAL_FRACTION: f32 = 0.10;
/// Floor on a steal, so robbing a player on zero is still worth doing.
pub const STEAL_MIN: u32 = 100;
/// How long the victim of a steal cannot move.
pub const STUN_SECS: f32 = 3.0;
/// How long the victim cannot be stolen from again. Longer than the stun on
/// purpose: without it the thief simply waits out the stun and robs again.
pub const STEAL_IMMUNITY_SECS: f32 = 5.0;

/// Freeze after losing a life, before respawning.
pub const DEATH_FREEZE_SECS: f32 = 1.5;
/// Ghosts scatter for this long after someone dies, to give the respawn air.
pub const SCATTER_AFTER_DEATH_SECS: f32 = 3.0;

pub const LIVES: u8 = 3;

/// Distance in tiles at which two things on the board count as touching.
pub const COLLIDE_RADIUS: f32 = 0.6;

/// The immunity window must outlast the stun, otherwise the thief simply waits
/// out the stun and robs again. Checked at compile time so the invariant cannot
/// be broken by editing one constant without the other.
const _: () = assert!(STEAL_IMMUNITY_SECS > STUN_SECS);

/// Points for the n-th ghost of one energization, zero indexed.
pub fn ghost_points(combo: usize) -> u32 {
    GHOST_CHAIN[combo.min(GHOST_CHAIN.len() - 1)]
}

/// The nominal size of a steal against a victim sitting on `victim_score`.
/// The simulation clamps this to what the victim actually has.
pub fn steal_amount(victim_score: u32) -> u32 {
    ((victim_score as f32 * STEAL_FRACTION) as u32).max(STEAL_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ghost_chain_doubles_and_then_holds() {
        assert_eq!(ghost_points(0), 200);
        assert_eq!(ghost_points(1), 400);
        assert_eq!(ghost_points(2), 800);
        assert_eq!(ghost_points(3), 1600);
        assert_eq!(ghost_points(9), 1600, "the chain must not run off the end");
    }

    #[test]
    fn a_steal_takes_a_tenth() {
        assert_eq!(steal_amount(2000), 200);
        assert_eq!(steal_amount(10_000), 1000);
    }

    #[test]
    fn a_steal_has_a_floor() {
        assert_eq!(steal_amount(500), STEAL_MIN);
        assert_eq!(steal_amount(0), STEAL_MIN);
    }

    // The immunity-outlasts-stun invariant is enforced at compile time by the
    // `const _` assertion above, so it needs no runtime test.
}
