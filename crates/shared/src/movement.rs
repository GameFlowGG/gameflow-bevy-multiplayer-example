//! Grid movement.
//!
//! Everything on the board moves the same way: along a corridor, one tile at a
//! time, turning only when it lands exactly on a tile boundary. That constraint
//! is what makes client prediction trivial: given a position, a direction and a
//! speed, the next position is exact, with no floating point drift across the
//! network.

use glam::{IVec2, Vec2};
use serde::{Deserialize, Serialize};

use crate::maze::MAZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    pub const ALL: [Dir; 4] = [Dir::Up, Dir::Down, Dir::Left, Dir::Right];

    pub const fn delta(self) -> IVec2 {
        match self {
            // Screen space: y grows downward, so Up is negative y.
            Dir::Up => IVec2::new(0, -1),
            Dir::Down => IVec2::new(0, 1),
            Dir::Left => IVec2::new(-1, 0),
            Dir::Right => IVec2::new(1, 0),
        }
    }

    pub const fn opposite(self) -> Dir {
        match self {
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
        }
    }
}

/// Who is moving. Ghosts may cross the house door, Pac-Man may not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mover {
    Pacman,
    Ghost,
}

impl Mover {
    fn can_enter(self, tile: IVec2) -> bool {
        match self {
            Mover::Pacman => MAZE.walkable_by_pacman(tile),
            Mover::Ghost => MAZE.walkable_by_ghost(tile),
        }
    }
}

/// A position on the grid: which tile, how far into it, and heading where.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GridPos {
    pub tile: IVec2,
    /// Progress into the current tile along `dir`, in `[0.0, 1.0)`.
    pub offset: f32,
    pub dir: Dir,
}

impl GridPos {
    pub fn new(tile: IVec2, dir: Dir) -> Self {
        GridPos {
            tile,
            offset: 0.0,
            dir,
        }
    }

    /// Advances along `dir`, adopting `desired` at each tile boundary when that
    /// way is open. Returns the number of tile boundaries crossed, which the
    /// caller uses to know when to test for pellets.
    ///
    /// Blocked movement parks the mover at `offset == 0.0` on its current tile
    /// rather than letting it drift into a wall.
    pub fn advance(&mut self, desired: Option<Dir>, mover: Mover, speed: f32, dt: f32) -> u32 {
        // A mover standing still may still turn in place, as long as the new
        // direction is open. This is what makes controls feel responsive when
        // you are pinned against a wall.
        if let Some(want) = desired {
            if self.offset == 0.0 && want != self.dir && mover.can_enter(self.next_tile(want)) {
                self.dir = want;
            }
        }

        let mut crossed = 0;
        let mut remaining = speed * dt;

        while remaining > 0.0 {
            // Refuse to leave the tile when the way ahead is closed.
            if !mover.can_enter(self.next_tile(self.dir)) {
                self.offset = 0.0;
                break;
            }

            let to_boundary = 1.0 - self.offset;
            if remaining < to_boundary {
                self.offset += remaining;
                break;
            }

            // Land exactly on the next tile, then decide where to go from here.
            remaining -= to_boundary;
            self.tile = self.next_tile(self.dir);
            self.offset = 0.0;
            crossed += 1;

            if let Some(want) = desired {
                if want != self.dir && mover.can_enter(self.next_tile(want)) {
                    self.dir = want;
                }
            }
        }

        crossed
    }

    /// The tile one step away in `d`, tunnel wrapping applied.
    pub fn next_tile(&self, d: Dir) -> IVec2 {
        MAZE.wrap(self.tile + d.delta())
    }

    /// Continuous position in tile units, for rendering and for distance checks
    /// that should not snap to the grid.
    pub fn world(&self) -> Vec2 {
        let d = self.dir.delta();
        Vec2::new(
            self.tile.x as f32 + d.x as f32 * self.offset,
            self.tile.y as f32 + d.y as f32 * self.offset,
        )
    }

    /// The tile this position counts as occupying for collisions and pickups.
    /// Past the halfway mark it is the tile ahead, which keeps head-on
    /// collisions from resolving a frame late.
    pub fn occupied_tile(&self) -> IVec2 {
        if self.offset >= 0.5 {
            self.next_tile(self.dir)
        } else {
            self.tile
        }
    }

    /// Reverses direction in place, preserving the distance already travelled.
    /// Used when ghosts flip on a mode change.
    pub fn reverse(&mut self) {
        if self.offset > 0.0 {
            self.tile = self.next_tile(self.dir);
            self.offset = 1.0 - self.offset;
        }
        self.dir = self.dir.opposite();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maze::{SPAWN_P0, TUNNEL_ROW};

    const SPEED: f32 = 8.0;

    fn open_pos() -> GridPos {
        GridPos::new(SPAWN_P0, Dir::Right)
    }

    #[test]
    fn opposite_is_an_involution() {
        for d in Dir::ALL {
            assert_eq!(d.opposite().opposite(), d);
            assert_eq!(d.delta() + d.opposite().delta(), IVec2::ZERO);
        }
    }

    #[test]
    fn advancing_a_full_tile_moves_exactly_one_tile() {
        let mut p = open_pos();
        let start = p.tile;
        let crossed = p.advance(None, Mover::Pacman, SPEED, 1.0 / SPEED);
        assert_eq!(crossed, 1);
        assert_eq!(p.tile, start + IVec2::X);
        assert!(p.offset.abs() < 1e-5, "offset was {}", p.offset);
    }

    #[test]
    fn advancing_half_a_tile_stays_in_the_tile() {
        let mut p = open_pos();
        let start = p.tile;
        let crossed = p.advance(None, Mover::Pacman, SPEED, 0.5 / SPEED);
        assert_eq!(crossed, 0);
        assert_eq!(p.tile, start);
        assert!((p.offset - 0.5).abs() < 1e-5);
    }

    #[test]
    fn turning_does_not_happen_mid_tile() {
        let mut p = open_pos();
        p.advance(None, Mover::Pacman, SPEED, 0.25 / SPEED);
        // Up is open from the spawn row, but we are not on a boundary yet.
        p.advance(Some(Dir::Up), Mover::Pacman, SPEED, 0.25 / SPEED);
        assert_eq!(p.dir, Dir::Right, "turned in the middle of a tile");
    }

    #[test]
    fn turning_happens_when_landing_on_a_boundary() {
        let mut p = GridPos::new(IVec2::new(6, 23), Dir::Right);
        // (6,23) has an open corridor upward at (6,22).
        assert!(MAZE.walkable_by_pacman(IVec2::new(6, 22)));
        p.offset = 0.0;
        p.advance(Some(Dir::Up), Mover::Pacman, SPEED, 0.0);
        assert_eq!(p.dir, Dir::Up, "should turn in place when stopped");
    }

    #[test]
    fn walking_into_a_wall_parks_at_the_boundary() {
        // (1,1) is a corridor with a wall above it.
        let mut p = GridPos::new(IVec2::new(1, 1), Dir::Up);
        assert!(!MAZE.walkable_by_pacman(IVec2::new(1, 0)));
        let crossed = p.advance(None, Mover::Pacman, SPEED, 1.0);
        assert_eq!(crossed, 0);
        assert_eq!(p.offset, 0.0);
        assert_eq!(p.tile, IVec2::new(1, 1));
    }

    #[test]
    fn a_long_step_crosses_several_tiles() {
        let mut p = GridPos::new(IVec2::new(1, 29), Dir::Right);
        let crossed = p.advance(None, Mover::Pacman, SPEED, 3.0 / SPEED);
        assert_eq!(crossed, 3);
        assert_eq!(p.tile, IVec2::new(4, 29));
    }

    #[test]
    fn crossing_the_tunnel_wraps_to_the_other_side() {
        let mut p = GridPos::new(IVec2::new(0, TUNNEL_ROW), Dir::Left);
        p.advance(None, Mover::Pacman, SPEED, 1.0 / SPEED);
        assert_eq!(p.tile.x, crate::maze::MAZE_W as i32 - 1);
        assert_eq!(p.tile.y, TUNNEL_ROW);
    }

    #[test]
    fn pacman_cannot_walk_through_the_ghost_door() {
        let mut p = GridPos::new(crate::maze::GHOST_DOOR, Dir::Down);
        let crossed = p.advance(None, Mover::Pacman, SPEED, 1.0);
        assert_eq!(crossed, 0, "pacman entered the ghost house");
    }

    #[test]
    fn ghosts_can_walk_through_the_ghost_door() {
        let mut p = GridPos::new(crate::maze::GHOST_DOOR, Dir::Down);
        let crossed = p.advance(None, Mover::Ghost, SPEED, 1.0 / SPEED);
        assert_eq!(crossed, 1);
    }

    #[test]
    fn occupied_tile_flips_past_the_halfway_mark() {
        let mut p = open_pos();
        p.offset = 0.49;
        assert_eq!(p.occupied_tile(), p.tile);
        p.offset = 0.51;
        assert_eq!(p.occupied_tile(), p.tile + IVec2::X);
    }

    #[test]
    fn reverse_preserves_travelled_distance() {
        let mut p = open_pos();
        p.offset = 0.25;
        let before = p.world();
        p.reverse();
        assert_eq!(p.dir, Dir::Left);
        let after = p.world();
        assert!(
            (before - after).length() < 1e-5,
            "reverse teleported: {before:?} -> {after:?}"
        );
    }

    /// The property that makes prediction work: the same inputs applied in one
    /// big step and in many small steps must land in the same place.
    #[test]
    fn movement_is_step_size_independent() {
        let mut coarse = GridPos::new(IVec2::new(1, 29), Dir::Right);
        coarse.advance(None, Mover::Pacman, SPEED, 4.0 / SPEED);

        let mut fine = GridPos::new(IVec2::new(1, 29), Dir::Right);
        for _ in 0..40 {
            fine.advance(None, Mover::Pacman, SPEED, 0.1 / SPEED);
        }

        assert_eq!(coarse.tile, fine.tile);
        assert!((coarse.offset - fine.offset).abs() < 1e-3);
    }
}
