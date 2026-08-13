//! The pellet field and its continuous drip.
//!
//! The maze never empties. Pellets trickle back into tiles that were eaten,
//! which is what keeps the player who is behind from starving while the leader
//! farms an empty board.

use glam::IVec2;
use serde::{Deserialize, Serialize};

use crate::maze::{chebyshev, MAZE, MAZE_H, MAZE_W};

/// Pellets respawned per second.
pub const DRIP_PER_SEC: f32 = 3.0;
/// A drip never lands this close to a live runner, so nobody is fed for free.
pub const DRIP_MIN_DISTANCE: i32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PelletKind {
    Normal,
    Power,
}

/// A small deterministic generator. The simulation needs reproducible
/// randomness and nothing more, so this avoids pulling `rand` into a crate that
/// both ends of the wire depend on.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn seeded(seed: u64) -> Self {
        // Zero is a fixed point of xorshift, so shift it out of the way.
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, n)`. Returns 0 when `n` is 0.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
}

const CELLS: usize = MAZE_W * MAZE_H;

#[derive(Debug, Clone)]
pub struct PelletField {
    cells: [Option<PelletKind>; CELLS],
    /// Fractional carry so a 3-per-second rate survives a 30Hz tick.
    drip_carry: f32,
    corridors: Vec<IVec2>,
}

fn index(tile: IVec2) -> Option<usize> {
    if tile.x < 0 || tile.y < 0 || tile.x >= MAZE_W as i32 || tile.y >= MAZE_H as i32 {
        return None;
    }
    Some(tile.y as usize * MAZE_W + tile.x as usize)
}

impl PelletField {
    /// Every corridor tile seeded with a normal pellet.
    pub fn new_full() -> Self {
        let corridors = MAZE.corridors();
        let mut cells = [None; CELLS];
        for tile in &corridors {
            if let Some(i) = index(*tile) {
                cells[i] = Some(PelletKind::Normal);
            }
        }
        PelletField {
            cells,
            drip_carry: 0.0,
            corridors,
        }
    }

    pub fn empty() -> Self {
        PelletField {
            cells: [None; CELLS],
            drip_carry: 0.0,
            corridors: MAZE.corridors(),
        }
    }

    pub fn at(&self, tile: IVec2) -> Option<PelletKind> {
        index(tile).and_then(|i| self.cells[i])
    }

    /// Removes and returns whatever sits on the tile.
    pub fn take(&mut self, tile: IVec2) -> Option<PelletKind> {
        let i = index(tile)?;
        self.cells[i].take()
    }

    pub fn put(&mut self, tile: IVec2, kind: PelletKind) {
        if let Some(i) = index(tile) {
            self.cells[i] = Some(kind);
        }
    }

    pub fn count(&self) -> usize {
        self.cells.iter().filter(|c| c.is_some()).count()
    }

    /// Respawns pellets for `dt` seconds. `players` are the tiles of every live
    /// runners; candidates near them are rejected.
    pub fn drip(&mut self, rng: &mut Rng, players: &[IVec2], dt: f32) -> Vec<(IVec2, PelletKind)> {
        self.drip_carry += DRIP_PER_SEC * dt;
        let mut spawned = Vec::new();

        while self.drip_carry >= 1.0 {
            self.drip_carry -= 1.0;
            match self.pick_free_tile(rng, players) {
                Some(tile) => {
                    self.put(tile, PelletKind::Normal);
                    spawned.push((tile, PelletKind::Normal));
                }
                // A full board is not an error, it just means nothing to drip.
                None => break,
            }
        }

        spawned
    }

    /// Places one power pellet. Returns where it landed.
    pub fn spawn_power(&mut self, rng: &mut Rng, players: &[IVec2]) -> Option<IVec2> {
        let tile = self.pick_free_tile(rng, players)?;
        self.put(tile, PelletKind::Power);
        Some(tile)
    }

    /// Rejection sampling over corridor tiles. Bounded attempts, then a linear
    /// scan so a nearly full board still terminates.
    fn pick_free_tile(&self, rng: &mut Rng, players: &[IVec2]) -> Option<IVec2> {
        let n = self.corridors.len();
        if n == 0 {
            return None;
        }

        for _ in 0..64 {
            let tile = self.corridors[rng.below(n)];
            if self.is_free_for_drip(tile, players) {
                return Some(tile);
            }
        }

        let start = rng.below(n);
        (0..n)
            .map(|k| self.corridors[(start + k) % n])
            .find(|t| self.is_free_for_drip(*t, players))
    }

    fn is_free_for_drip(&self, tile: IVec2, players: &[IVec2]) -> bool {
        self.at(tile).is_none() && players.iter().all(|p| chebyshev(tile, *p) >= DRIP_MIN_DISTANCE)
    }

    /// The whole field as two bits per corridor tile, for the initial sync.
    /// Order matches `MAZE.corridors()`, so the client can rebuild it without
    /// receiving coordinates.
    pub fn snapshot_bits(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.corridors.len().div_ceil(4)];
        for (i, tile) in self.corridors.iter().enumerate() {
            let code = match self.at(*tile) {
                None => 0u8,
                Some(PelletKind::Normal) => 1,
                Some(PelletKind::Power) => 2,
            };
            out[i / 4] |= code << ((i % 4) * 2);
        }
        out
    }

    /// Rebuilds a field from `snapshot_bits`.
    pub fn from_snapshot_bits(bits: &[u8]) -> Self {
        let mut field = PelletField::empty();
        let corridors = field.corridors.clone();
        for (i, tile) in corridors.iter().enumerate() {
            let byte = match bits.get(i / 4) {
                Some(b) => *b,
                None => break,
            };
            match (byte >> ((i % 4) * 2)) & 0b11 {
                1 => field.put(*tile, PelletKind::Normal),
                2 => field.put(*tile, PelletKind::Power),
                _ => {}
            }
        }
        field
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_corridor() -> IVec2 {
        MAZE.corridors()[0]
    }

    #[test]
    fn rng_is_deterministic_for_a_seed() {
        let a: Vec<u64> = (0..8).map(|_| Rng::seeded(7).next_u64()).collect();
        let mut b = Rng::seeded(7);
        assert_eq!(a[0], b.next_u64());

        let mut c = Rng::seeded(7);
        let mut d = Rng::seeded(7);
        for _ in 0..100 {
            assert_eq!(c.next_u64(), d.next_u64());
        }
    }

    #[test]
    fn rng_seeds_differ() {
        let mut a = Rng::seeded(1);
        let mut b = Rng::seeded(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn rng_below_stays_in_range() {
        let mut rng = Rng::seeded(99);
        for _ in 0..1000 {
            assert!(rng.below(7) < 7);
        }
        assert_eq!(rng.below(0), 0);
    }

    #[test]
    fn a_full_field_has_a_pellet_on_every_corridor() {
        let field = PelletField::new_full();
        assert_eq!(field.count(), MAZE.corridors().len());
        for tile in MAZE.corridors() {
            assert_eq!(field.at(tile), Some(PelletKind::Normal));
        }
    }

    #[test]
    fn taking_a_pellet_empties_the_tile() {
        let mut field = PelletField::new_full();
        let t = first_corridor();
        assert_eq!(field.take(t), Some(PelletKind::Normal));
        assert_eq!(field.at(t), None);
        assert_eq!(field.take(t), None, "taking twice must not resurrect it");
    }

    #[test]
    fn walls_never_hold_pellets() {
        let field = PelletField::new_full();
        assert_eq!(field.at(IVec2::new(0, 0)), None);
    }

    #[test]
    fn drip_rate_is_three_per_second() {
        let mut field = PelletField::empty();
        let mut rng = Rng::seeded(1);
        assert_eq!(field.drip(&mut rng, &[], 1.0).len(), 3);
    }

    #[test]
    fn drip_accumulates_across_short_ticks() {
        let mut field = PelletField::empty();
        let mut rng = Rng::seeded(1);
        let mut total = 0;
        // 30 ticks of 1/30s is one second, so exactly three pellets.
        for _ in 0..30 {
            total += field.drip(&mut rng, &[], 1.0 / 30.0).len();
        }
        assert_eq!(total, 3);
    }

    #[test]
    fn drip_never_lands_near_a_live_player() {
        let mut field = PelletField::empty();
        let mut rng = Rng::seeded(42);
        let player = IVec2::new(6, 23);
        let spawned = field.drip(&mut rng, &[player], 30.0);
        assert!(!spawned.is_empty());
        for (tile, _) in spawned {
            assert!(
                chebyshev(tile, player) >= DRIP_MIN_DISTANCE,
                "{tile:?} dripped {} tiles from the player",
                chebyshev(tile, player)
            );
        }
    }

    #[test]
    fn drip_stops_when_the_board_is_full() {
        let mut field = PelletField::new_full();
        let mut rng = Rng::seeded(3);
        assert!(field.drip(&mut rng, &[], 100.0).is_empty());
    }

    #[test]
    fn power_pellets_land_on_empty_tiles_away_from_players() {
        let mut field = PelletField::empty();
        let mut rng = Rng::seeded(11);
        let player = IVec2::new(6, 23);
        let tile = field.spawn_power(&mut rng, &[player]).unwrap();
        assert_eq!(field.at(tile), Some(PelletKind::Power));
        assert!(chebyshev(tile, player) >= DRIP_MIN_DISTANCE);
    }

    #[test]
    fn snapshot_bits_round_trip() {
        let mut field = PelletField::new_full();
        let corridors = MAZE.corridors();
        field.take(corridors[0]);
        field.take(corridors[5]);
        field.put(corridors[9], PelletKind::Power);

        let restored = PelletField::from_snapshot_bits(&field.snapshot_bits());
        for tile in &corridors {
            assert_eq!(restored.at(*tile), field.at(*tile), "mismatch at {tile:?}");
        }
    }

    #[test]
    fn snapshot_bits_are_compact() {
        let field = PelletField::new_full();
        let bits = field.snapshot_bits();
        assert!(bits.len() < 100, "pellet sync grew to {} bytes", bits.len());
    }
}
