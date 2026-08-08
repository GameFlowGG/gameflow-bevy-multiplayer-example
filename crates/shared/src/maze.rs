//! The maze grid.
//!
//! One fixed 28x31 layout, compiled in. Row 14 is the tunnel row: walking off
//! either side wraps to the other.

use glam::IVec2;

pub const MAZE_W: usize = 28;
pub const MAZE_H: usize = 31;

/// The row whose left and right edges connect to each other.
pub const TUNNEL_ROW: i32 = 14;

/// Where slot 0 and slot 1 start, and where they respawn after dying.
pub const SPAWN_P0: IVec2 = IVec2::new(6, 23);
pub const SPAWN_P1: IVec2 = IVec2::new(21, 23);

/// The tile ghosts are released from and return to when eaten.
pub const GHOST_HOUSE: IVec2 = IVec2::new(13, 14);
/// The tile directly above the house door, where a released ghost enters play.
pub const GHOST_DOOR: IVec2 = IVec2::new(13, 11);

/// Scatter targets, indexed by ghost slot. Corners are intentionally outside
/// the walkable area: ghosts orbit them rather than reaching them, which is
/// what produces the classic patrol behaviour.
pub const SCATTER_CORNERS: [IVec2; 7] = [
    IVec2::new(26, 1),
    IVec2::new(1, 1),
    IVec2::new(26, 29),
    IVec2::new(1, 29),
    IVec2::new(13, 1),
    IVec2::new(1, 15),
    IVec2::new(26, 15),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tile {
    Wall,
    Corridor,
    /// Inside the ghost house. Ghosts may walk here, Pac-Man may not.
    House,
    /// The house door. Ghosts cross it, Pac-Man may not.
    Door,
    /// Outside the playfield.
    Void,
}

impl Tile {
    /// Whether a Pac-Man may occupy this tile.
    pub const fn walkable_by_pacman(self) -> bool {
        matches!(self, Tile::Corridor)
    }

    /// Whether a ghost may occupy this tile.
    pub const fn walkable_by_ghost(self) -> bool {
        matches!(self, Tile::Corridor | Tile::House | Tile::Door)
    }

    /// Whether a pellet may ever sit here.
    pub const fn holds_pellets(self) -> bool {
        matches!(self, Tile::Corridor)
    }
}

/// `#` wall, `.` corridor, `-` house door, `_` house interior, ` ` void.
const LAYOUT: [&str; MAZE_H] = [
    "############################",
    "#............##............#",
    "#.####.#####.##.#####.####.#",
    "#.####.#####.##.#####.####.#",
    "#.####.#####.##.#####.####.#",
    "#..........................#",
    "#.####.##.########.##.####.#",
    "#.####.##.########.##.####.#",
    // A corridor runs down the middle from row 8 to the house door, so the
    // ghosts released from the house have a way into the maze.
    "#......##..........##......#",
    "######.######..######.######",
    "     #.######..######.#     ",
    "     #.######..######.#     ",
    "     #.######--######.#     ",
    "######.## #______# ##.######",
    ".......   #______#   .......",
    "######.## #______# ##.######",
    "     #.## ######## ##.#     ",
    "     #.##          ##.#     ",
    "     #.## ######## ##.#     ",
    "######.## ######## ##.######",
    "#............##............#",
    "#.####.#####.##.#####.####.#",
    "#.####.#####.##.#####.####.#",
    "#...##................##...#",
    "###.##.##.########.##.##.###",
    "###.##.##.########.##.##.###",
    "#......##....##....##......#",
    "#.##########.##.##########.#",
    "#.##########.##.##########.#",
    "#..........................#",
    "############################",
];

pub struct Maze;

/// The one and only maze.
pub const MAZE: Maze = Maze;

impl Maze {
    pub fn tile(&self, x: i32, y: i32) -> Tile {
        if y < 0 || y >= MAZE_H as i32 || x < 0 || x >= MAZE_W as i32 {
            return Tile::Void;
        }
        match LAYOUT[y as usize].as_bytes()[x as usize] {
            b'#' => Tile::Wall,
            b'.' => Tile::Corridor,
            b'-' => Tile::Door,
            b'_' => Tile::House,
            _ => Tile::Void,
        }
    }

    pub fn at(&self, pos: IVec2) -> Tile {
        self.tile(pos.x, pos.y)
    }

    /// Wraps the tunnel row horizontally. Everything else is returned as is.
    pub fn wrap(&self, pos: IVec2) -> IVec2 {
        if pos.y != TUNNEL_ROW {
            return pos;
        }
        let w = MAZE_W as i32;
        IVec2::new(pos.x.rem_euclid(w), pos.y)
    }

    pub fn walkable_by_pacman(&self, pos: IVec2) -> bool {
        self.at(self.wrap(pos)).walkable_by_pacman()
    }

    pub fn walkable_by_ghost(&self, pos: IVec2) -> bool {
        self.at(self.wrap(pos)).walkable_by_ghost()
    }

    /// Every corridor tile, in row-major order. Used to seed the pellet field
    /// and to pick drip candidates.
    pub fn corridors(&self) -> Vec<IVec2> {
        let mut out = Vec::with_capacity(MAZE_W * MAZE_H / 3);
        for y in 0..MAZE_H as i32 {
            for x in 0..MAZE_W as i32 {
                if self.tile(x, y).holds_pellets() {
                    out.push(IVec2::new(x, y));
                }
            }
        }
        out
    }
}

/// Chebyshev distance, the natural metric on a grid where diagonal steps do not
/// exist but proximity should still be measured as a square.
pub fn chebyshev(a: IVec2, b: IVec2) -> i32 {
    (a.x - b.x).abs().max((a.y - b.y).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_exactly_maze_width() {
        for (y, row) in LAYOUT.iter().enumerate() {
            assert_eq!(row.len(), MAZE_W, "row {y} is {} chars", row.len());
        }
    }

    /// Outside the tunnel row nothing may leave the board. The edge is either a
    /// wall or void; both are equally impassable, which is what actually
    /// matters.
    #[test]
    fn nothing_can_walk_off_the_board_except_through_the_tunnel() {
        for y in 0..MAZE_H as i32 {
            if y == TUNNEL_ROW {
                continue;
            }
            let left = IVec2::new(0, y);
            let right = IVec2::new(MAZE_W as i32 - 1, y);
            assert!(!MAZE.walkable_by_ghost(left), "left edge open at y={y}");
            assert!(!MAZE.walkable_by_ghost(right), "right edge open at y={y}");
        }
    }

    #[test]
    fn top_and_bottom_rows_are_solid_wall() {
        for x in 0..MAZE_W as i32 {
            assert_eq!(MAZE.tile(x, 0), Tile::Wall);
            assert_eq!(MAZE.tile(x, MAZE_H as i32 - 1), Tile::Wall);
        }
    }

    #[test]
    fn tunnel_row_is_open_on_both_edges() {
        assert!(MAZE.walkable_by_pacman(IVec2::new(0, TUNNEL_ROW)));
        assert!(MAZE.walkable_by_pacman(IVec2::new(MAZE_W as i32 - 1, TUNNEL_ROW)));
    }

    #[test]
    fn tunnel_row_wraps_horizontally() {
        let off_left = IVec2::new(-1, TUNNEL_ROW);
        assert_eq!(MAZE.wrap(off_left), IVec2::new(MAZE_W as i32 - 1, TUNNEL_ROW));

        let off_right = IVec2::new(MAZE_W as i32, TUNNEL_ROW);
        assert_eq!(MAZE.wrap(off_right), IVec2::new(0, TUNNEL_ROW));
    }

    #[test]
    fn other_rows_do_not_wrap() {
        let off = IVec2::new(-1, 5);
        assert_eq!(MAZE.wrap(off), off);
        assert!(!MAZE.walkable_by_pacman(off));
    }

    #[test]
    fn spawns_are_walkable_and_far_apart() {
        assert!(MAZE.walkable_by_pacman(SPAWN_P0));
        assert!(MAZE.walkable_by_pacman(SPAWN_P1));
        assert!(
            chebyshev(SPAWN_P0, SPAWN_P1) >= 10,
            "spawns must not start on top of each other"
        );
    }

    #[test]
    fn pacman_cannot_enter_the_ghost_house() {
        assert!(!MAZE.walkable_by_pacman(GHOST_HOUSE));
        assert!(MAZE.walkable_by_ghost(GHOST_HOUSE));
        assert!(MAZE.walkable_by_ghost(IVec2::new(13, 12)));
        assert!(!MAZE.walkable_by_pacman(IVec2::new(13, 12)));
    }

    #[test]
    fn the_ghost_door_leads_to_a_corridor() {
        assert!(MAZE.walkable_by_ghost(GHOST_DOOR));
        assert!(MAZE.walkable_by_pacman(GHOST_DOOR));
    }

    /// Regression: the first layout sealed the ghost house, so every ghost
    /// spawned trapped and the match was unplayable.
    #[test]
    fn ghosts_can_reach_the_maze_from_inside_the_house() {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![GHOST_HOUSE];
        seen.insert(GHOST_HOUSE);
        while let Some(cur) = stack.pop() {
            for d in [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y] {
                let next = MAZE.wrap(cur + d);
                if MAZE.walkable_by_ghost(next) && seen.insert(next) {
                    stack.push(next);
                }
            }
        }
        assert!(seen.contains(&GHOST_DOOR), "no path from the house to the door");
        assert!(seen.contains(&SPAWN_P0), "ghosts cannot reach slot 0 spawn");
        assert!(seen.contains(&SPAWN_P1), "ghosts cannot reach slot 1 spawn");
    }

    #[test]
    fn the_maze_has_a_reasonable_number_of_corridors() {
        let n = MAZE.corridors().len();
        assert!(n > 200, "only {n} corridor tiles, layout looks broken");
    }

    /// Every corridor tile must be reachable from a spawn, otherwise pellets
    /// would drip into pockets nobody can ever eat.
    #[test]
    fn all_corridors_are_reachable_from_spawn() {
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![SPAWN_P0];
        seen.insert(SPAWN_P0);
        while let Some(cur) = stack.pop() {
            for d in [IVec2::X, IVec2::NEG_X, IVec2::Y, IVec2::NEG_Y] {
                let next = MAZE.wrap(cur + d);
                if MAZE.walkable_by_pacman(next) && seen.insert(next) {
                    stack.push(next);
                }
            }
        }
        let unreachable: Vec<_> = MAZE
            .corridors()
            .into_iter()
            .filter(|t| !seen.contains(t))
            .collect();
        assert!(unreachable.is_empty(), "unreachable corridors: {unreachable:?}");
    }
}
