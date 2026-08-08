//! Ghost behaviour.
//!
//! Each ghost picks a target tile every time it lands on a tile boundary, then
//! walks the neighbour that gets it closest. Different targets are the whole
//! personality system: the movement code underneath is identical for all of
//! them.
//!
//! With two Pac-Man on the board a ghost hunts whichever live one is nearest.
//! When only one is left, every ghost converges on the survivor, which is what
//! stops an empty maze from being a reward for outlasting your rival.

use glam::IVec2;
use serde::{Deserialize, Serialize};

use crate::difficulty::{GHOST_EATEN_MULT, GHOST_FRIGHTENED_MULT};
use crate::maze::{chebyshev, GHOST_DOOR, GHOST_HOUSE, MAZE, SCATTER_CORNERS};
use crate::movement::{Dir, GridPos};

/// How long a ghost patrols its corner before hunting again.
pub const SCATTER_SECONDS: f32 = 7.0;
/// How long it hunts before patrolling again.
pub const CHASE_SECONDS: f32 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GhostMode {
    Scatter,
    Chase,
    Frightened,
    /// Eyes returning to the house after being eaten.
    Eaten,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Personality {
    /// Goes straight for the tile you are on.
    Red,
    /// Aims four tiles ahead of you, to cut you off.
    Pink,
    /// Aims at the reflection of Red about a point ahead of you, which makes it
    /// pincer when Red is close and wander when Red is far.
    Cyan,
    /// Hunts from a distance, but loses its nerve and goes home when close.
    Orange,
}

impl Personality {
    /// Ghost slot to personality. Slots 4 to 6 arrive with the difficulty curve
    /// and reuse the aggressive three.
    pub fn for_slot(slot: usize) -> Personality {
        match slot % 4 {
            0 => Personality::Red,
            1 => Personality::Pink,
            2 => Personality::Cyan,
            _ => Personality::Orange,
        }
    }
}

/// What a ghost knows about a Pac-Man when choosing a target.
#[derive(Debug, Clone, Copy)]
pub struct PacTarget {
    pub tile: IVec2,
    pub dir: Dir,
    pub alive: bool,
}

#[derive(Debug, Clone)]
pub struct Ghost {
    pub pos: GridPos,
    pub mode: GhostMode,
    pub personality: Personality,
    pub scatter_corner: IVec2,
    /// Seconds left in the current scatter or chase phase.
    pub mode_timer: f32,
    /// Set while the ghost is still waiting inside the house.
    pub penned: bool,
}

impl Ghost {
    pub fn new(slot: usize) -> Ghost {
        Ghost {
            pos: GridPos::new(GHOST_HOUSE, Dir::Up),
            mode: GhostMode::Scatter,
            personality: Personality::for_slot(slot),
            scatter_corner: SCATTER_CORNERS[slot % SCATTER_CORNERS.len()],
            mode_timer: SCATTER_SECONDS,
            penned: true,
        }
    }

    pub fn speed_multiplier(&self) -> f32 {
        match self.mode {
            GhostMode::Frightened => GHOST_FRIGHTENED_MULT,
            GhostMode::Eaten => GHOST_EATEN_MULT,
            _ => 1.0,
        }
    }

    /// Where this ghost wants to be right now.
    pub fn target(&self, pacmen: &[PacTarget], red_tile: IVec2) -> IVec2 {
        if self.mode == GhostMode::Eaten {
            return GHOST_HOUSE;
        }
        if self.penned {
            return GHOST_DOOR;
        }
        if self.mode == GhostMode::Scatter {
            return self.scatter_corner;
        }

        let Some(prey) = self.nearest_live(pacmen) else {
            return self.scatter_corner;
        };

        match self.personality {
            Personality::Red => prey.tile,
            Personality::Pink => prey.tile + prey.dir.delta() * 4,
            Personality::Cyan => {
                let pivot = prey.tile + prey.dir.delta() * 2;
                pivot * 2 - red_tile
            }
            Personality::Orange => {
                if chebyshev(self.pos.tile, prey.tile) > 8 {
                    prey.tile
                } else {
                    self.scatter_corner
                }
            }
        }
    }

    fn nearest_live(&self, pacmen: &[PacTarget]) -> Option<PacTarget> {
        pacmen
            .iter()
            .filter(|p| p.alive)
            .min_by_key(|p| chebyshev(self.pos.tile, p.tile))
            .copied()
    }

    /// Advances toward `target`, re-choosing a direction at every tile boundary.
    pub fn step(&mut self, target: IVec2, speed: f32, dt: f32) {
        let mut remaining = speed * dt;

        // Bounded so a pathological layout can never hang the tick.
        for _ in 0..64 {
            if remaining <= 0.0 {
                break;
            }

            if self.pos.offset == 0.0 {
                if let Some(dir) = self.choose_dir(target) {
                    self.pos.dir = dir;
                }
            }

            let ahead = self.pos.next_tile(self.pos.dir);
            if !MAZE.walkable_by_ghost(ahead) {
                self.pos.offset = 0.0;
                break;
            }

            let to_boundary = 1.0 - self.pos.offset;
            if remaining < to_boundary {
                self.pos.offset += remaining;
                break;
            }

            remaining -= to_boundary;
            self.pos.tile = ahead;
            self.pos.offset = 0.0;

            if self.penned && self.pos.tile == GHOST_DOOR {
                self.penned = false;
            }
            if self.mode == GhostMode::Eaten && self.pos.tile == GHOST_HOUSE {
                self.mode = GhostMode::Scatter;
                self.mode_timer = SCATTER_SECONDS;
                self.penned = true;
            }
        }
    }

    /// Picks the neighbour that best serves the target. Ghosts never reverse
    /// mid-corridor, which is what keeps them committed to a route; the only
    /// exception is a dead end, where reversing is the sole legal move.
    fn choose_dir(&self, target: IVec2) -> Option<Dir> {
        let back = self.pos.dir.opposite();
        let mut options: Vec<Dir> = Dir::ALL
            .into_iter()
            .filter(|d| *d != back)
            .filter(|d| MAZE.walkable_by_ghost(self.pos.next_tile(*d)))
            .collect();

        if options.is_empty() {
            options.push(back);
            if !MAZE.walkable_by_ghost(self.pos.next_tile(back)) {
                return None;
            }
        }

        // Frightened ghosts flee: same rule, opposite preference.
        let flee = self.mode == GhostMode::Frightened;
        options
            .into_iter()
            .map(|d| {
                let tile = self.pos.next_tile(d);
                let dx = (tile.x - target.x) as f32;
                let dy = (tile.y - target.y) as f32;
                (d, dx * dx + dy * dy)
            })
            .reduce(|best, cur| {
                let better = if flee { cur.1 > best.1 } else { cur.1 < best.1 };
                if better {
                    cur
                } else {
                    best
                }
            })
            .map(|(d, _)| d)
    }

    /// Advances the scatter and chase clock. Frightened and eaten ghosts are
    /// driven by the simulation instead, so they are left alone here.
    pub fn tick_mode(&mut self, dt: f32) {
        if matches!(self.mode, GhostMode::Frightened | GhostMode::Eaten) {
            return;
        }
        self.mode_timer -= dt;
        if self.mode_timer <= 0.0 {
            self.mode = match self.mode {
                GhostMode::Scatter => GhostMode::Chase,
                _ => GhostMode::Scatter,
            };
            self.mode_timer = match self.mode {
                GhostMode::Scatter => SCATTER_SECONDS,
                _ => CHASE_SECONDS,
            };
            self.pos.reverse();
        }
    }

    pub fn frighten(&mut self) {
        // Eyes on their way home are not scared of anything.
        if self.mode == GhostMode::Eaten {
            return;
        }
        if self.mode != GhostMode::Frightened {
            self.pos.reverse();
        }
        self.mode = GhostMode::Frightened;
    }

    pub fn unfrighten(&mut self) {
        if self.mode == GhostMode::Frightened {
            self.mode = GhostMode::Chase;
            self.mode_timer = CHASE_SECONDS;
        }
    }

    pub fn eat(&mut self) {
        self.mode = GhostMode::Eaten;
    }

    /// Sent back to the pen without being eaten, after a Pac-Man dies.
    pub fn reset_to_house(&mut self) {
        self.pos = GridPos::new(GHOST_HOUSE, Dir::Up);
        self.mode = GhostMode::Scatter;
        self.mode_timer = crate::score::SCATTER_AFTER_DEATH_SECS;
        self.penned = true;
    }

    pub fn is_edible(&self) -> bool {
        self.mode == GhostMode::Frightened
    }

    pub fn is_dangerous(&self) -> bool {
        matches!(self.mode, GhostMode::Scatter | GhostMode::Chase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ghost_at(personality: Personality, tile: IVec2) -> Ghost {
        let mut g = Ghost::new(0);
        g.personality = personality;
        g.pos = GridPos::new(tile, Dir::Right);
        g.mode = GhostMode::Chase;
        g.penned = false;
        g
    }

    fn live(tile: IVec2, dir: Dir) -> PacTarget {
        PacTarget {
            tile,
            dir,
            alive: true,
        }
    }

    #[test]
    fn red_targets_the_nearest_pacman_tile() {
        let g = ghost_at(Personality::Red, IVec2::new(1, 1));
        let near = live(IVec2::new(3, 1), Dir::Right);
        let far = live(IVec2::new(20, 20), Dir::Left);
        assert_eq!(g.target(&[near, far], IVec2::ZERO), IVec2::new(3, 1));
        assert_eq!(g.target(&[far, near], IVec2::ZERO), IVec2::new(3, 1));
    }

    #[test]
    fn pink_targets_four_tiles_ahead() {
        let g = ghost_at(Personality::Pink, IVec2::new(1, 1));
        let p = live(IVec2::new(10, 10), Dir::Up);
        let want = IVec2::new(10, 10) + Dir::Up.delta() * 4;
        assert_eq!(g.target(&[p], IVec2::ZERO), want);
    }

    #[test]
    fn cyan_reflects_red_about_a_point_ahead() {
        let g = ghost_at(Personality::Cyan, IVec2::new(1, 1));
        let p = live(IVec2::new(10, 10), Dir::Right);
        let red = IVec2::new(8, 10);
        let pivot = IVec2::new(12, 10);
        assert_eq!(g.target(&[p], red), pivot * 2 - red);
    }

    #[test]
    fn orange_chases_from_far_and_goes_home_when_close() {
        let g = ghost_at(Personality::Orange, IVec2::new(1, 1));
        let far = live(IVec2::new(20, 1), Dir::Up);
        assert_eq!(g.target(&[far], IVec2::ZERO), IVec2::new(20, 1));

        let near = live(IVec2::new(5, 1), Dir::Up);
        assert_eq!(g.target(&[near], IVec2::ZERO), g.scatter_corner);
    }

    #[test]
    fn dead_pacmen_are_never_targeted() {
        let g = ghost_at(Personality::Red, IVec2::new(1, 1));
        let dead = PacTarget {
            tile: IVec2::new(2, 1),
            dir: Dir::Up,
            alive: false,
        };
        let alive = live(IVec2::new(20, 20), Dir::Up);
        assert_eq!(g.target(&[dead, alive], IVec2::ZERO), IVec2::new(20, 20));
    }

    #[test]
    fn with_no_live_prey_a_ghost_falls_back_to_its_corner() {
        let g = ghost_at(Personality::Red, IVec2::new(1, 1));
        assert_eq!(g.target(&[], IVec2::ZERO), g.scatter_corner);
    }

    #[test]
    fn scatter_mode_ignores_pacman_entirely() {
        let mut g = ghost_at(Personality::Red, IVec2::new(1, 1));
        g.mode = GhostMode::Scatter;
        let p = live(IVec2::new(3, 1), Dir::Up);
        assert_eq!(g.target(&[p], IVec2::ZERO), g.scatter_corner);
    }

    #[test]
    fn eaten_ghosts_head_home_no_matter_what() {
        let mut g = ghost_at(Personality::Red, IVec2::new(1, 1));
        g.mode = GhostMode::Eaten;
        let p = live(IVec2::new(3, 1), Dir::Up);
        assert_eq!(g.target(&[p], IVec2::ZERO), GHOST_HOUSE);
    }

    #[test]
    fn a_penned_ghost_aims_for_the_door() {
        let g = Ghost::new(0);
        assert!(g.penned);
        assert_eq!(g.target(&[], IVec2::ZERO), GHOST_DOOR);
    }

    #[test]
    fn a_ghost_leaves_the_house_and_reaches_the_maze() {
        let mut g = Ghost::new(0);
        for _ in 0..600 {
            let target = g.target(&[], IVec2::ZERO);
            g.step(target, 6.5, 1.0 / 30.0);
            if !g.penned {
                break;
            }
        }
        assert!(!g.penned, "ghost never made it out of the house");
        assert!(MAZE.walkable_by_ghost(g.pos.tile));
    }

    #[test]
    fn a_ghost_closes_distance_on_a_stationary_target() {
        let mut g = ghost_at(Personality::Red, IVec2::new(6, 23));
        let prey = live(IVec2::new(21, 23), Dir::Right);
        let before = chebyshev(g.pos.tile, prey.tile);
        for _ in 0..200 {
            let target = g.target(&[prey], IVec2::ZERO);
            g.step(target, 6.5, 1.0 / 30.0);
        }
        let after = chebyshev(g.pos.tile, prey.tile);
        assert!(after < before, "ghost went from {before} to {after} tiles away");
    }

    #[test]
    fn a_ghost_never_ends_a_step_inside_a_wall() {
        let mut g = ghost_at(Personality::Red, IVec2::new(6, 23));
        let prey = live(IVec2::new(1, 1), Dir::Up);
        for _ in 0..2000 {
            let target = g.target(&[prey], IVec2::ZERO);
            g.step(target, 10.0, 1.0 / 30.0);
            assert!(
                MAZE.walkable_by_ghost(g.pos.tile),
                "ghost stepped into {:?}",
                g.pos.tile
            );
        }
    }

    #[test]
    fn frightened_ghosts_run_away_instead_of_closing_in() {
        let mut g = ghost_at(Personality::Red, IVec2::new(6, 20));
        g.mode = GhostMode::Frightened;
        let prey_tile = IVec2::new(6, 23);
        let before = chebyshev(g.pos.tile, prey_tile);
        for _ in 0..120 {
            g.step(prey_tile, 6.5 * GHOST_FRIGHTENED_MULT, 1.0 / 30.0);
        }
        let after = chebyshev(g.pos.tile, prey_tile);
        assert!(after > before, "frightened ghost closed in: {before} -> {after}");
    }

    #[test]
    fn mode_alternates_between_scatter_and_chase() {
        let mut g = ghost_at(Personality::Red, IVec2::new(6, 23));
        g.mode = GhostMode::Scatter;
        g.mode_timer = SCATTER_SECONDS;
        g.tick_mode(SCATTER_SECONDS + 0.1);
        assert_eq!(g.mode, GhostMode::Chase);
        g.tick_mode(CHASE_SECONDS + 0.1);
        assert_eq!(g.mode, GhostMode::Scatter);
    }

    #[test]
    fn frightened_and_eaten_ghosts_ignore_the_mode_clock() {
        let mut g = ghost_at(Personality::Red, IVec2::new(6, 23));
        g.mode = GhostMode::Frightened;
        g.tick_mode(1000.0);
        assert_eq!(g.mode, GhostMode::Frightened);

        g.mode = GhostMode::Eaten;
        g.tick_mode(1000.0);
        assert_eq!(g.mode, GhostMode::Eaten);
    }

    #[test]
    fn eyes_are_not_frightened_and_not_edible() {
        let mut g = ghost_at(Personality::Red, IVec2::new(6, 23));
        g.eat();
        assert_eq!(g.mode, GhostMode::Eaten);
        g.frighten();
        assert_eq!(g.mode, GhostMode::Eaten, "eyes should ignore a power pellet");
        assert!(!g.is_edible());
        assert!(!g.is_dangerous());
    }

    #[test]
    fn frightening_reverses_direction_once() {
        let mut g = ghost_at(Personality::Red, IVec2::new(6, 23));
        let before = g.pos.dir;
        g.frighten();
        assert_eq!(g.pos.dir, before.opposite());

        let after = g.pos.dir;
        g.frighten();
        assert_eq!(g.pos.dir, after, "a second power pellet must not flip again");
    }
}
