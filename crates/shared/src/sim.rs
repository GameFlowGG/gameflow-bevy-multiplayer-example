//! The authoritative match simulation.
//!
//! One `tick` is one 30Hz step. The server owns an instance of this and is the
//! only thing allowed to decide what happened; the client owns one too, but
//! only ever feeds its own runner through it to predict local movement.

use glam::{IVec2, Vec2};
use serde::{Deserialize, Serialize};

use crate::difficulty::{Difficulty, RUNNER_SPEED};
use crate::ghosts::{Ghost, GhostMode, RunnerTarget};
use crate::maze::{MAZE_W, SPAWN_P0, SPAWN_P1};
use crate::movement::{Dir, GridPos, Mover};
use crate::pellets::{PelletField, PelletKind, Rng};
use crate::score;
use crate::TICK_DT;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunnerState {
    Alive,
    /// Frozen after losing a life, waiting to respawn.
    Dying,
    /// Out of lives. Off the board for the rest of the match.
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchPhase {
    /// Built, waiting for the server to start it.
    Waiting,
    Running,
    /// `winner` is `None` on a draw.
    Finished { winner: Option<u8> },
}

#[derive(Debug, Clone)]
pub struct Runner {
    pub slot: u8,
    pub pos: GridPos,
    pub desired: Option<Dir>,
    pub state: RunnerState,
    pub lives: u8,
    pub score: u32,
    /// Absolute sim times, not durations.
    pub energized_until: f32,
    pub stunned_until: f32,
    pub immune_until: f32,
    pub respawn_at: f32,
    /// Ghosts eaten during the current energization.
    pub ghost_combo: usize,
}

impl Runner {
    fn new(slot: u8) -> Runner {
        let (tile, dir) = if slot == 0 {
            (SPAWN_P0, Dir::Right)
        } else {
            (SPAWN_P1, Dir::Left)
        };
        Runner {
            slot,
            pos: GridPos::new(tile, dir),
            desired: None,
            state: RunnerState::Alive,
            lives: score::LIVES,
            score: 0,
            energized_until: 0.0,
            stunned_until: 0.0,
            immune_until: 0.0,
            respawn_at: 0.0,
            ghost_combo: 0,
        }
    }

    pub fn is_energized(&self, now: f32) -> bool {
        self.state == RunnerState::Alive && now < self.energized_until
    }

    pub fn is_stunned(&self, now: f32) -> bool {
        now < self.stunned_until
    }

    pub fn is_immune(&self, now: f32) -> bool {
        now < self.immune_until
    }

    /// On the board and able to be hit, eat, or be eaten.
    pub fn is_active(&self) -> bool {
        self.state == RunnerState::Alive
    }

    fn can_move(&self, now: f32) -> bool {
        self.is_active() && !self.is_stunned(now)
    }

    fn spawn_tile(&self) -> IVec2 {
        if self.slot == 0 {
            SPAWN_P0
        } else {
            SPAWN_P1
        }
    }
}

/// Everything that happened during one tick. The server turns this into network
/// deltas; the client uses it for sound and effects.
#[derive(Debug, Default, Clone)]
pub struct TickEvents {
    /// tile, kind, who ate it
    pub pellets_eaten: Vec<(IVec2, PelletKind, u8)>,
    pub pellets_spawned: Vec<(IVec2, PelletKind)>,
    /// ghost index, who ate it, points awarded
    pub ghosts_eaten: Vec<(usize, u8, u32)>,
    /// who died
    pub deaths: Vec<u8>,
    /// thief, victim, points taken
    pub steals: Vec<(u8, u8, u32)>,
    /// who ran out of lives this tick
    pub eliminated: Vec<u8>,
}

pub struct Sim {
    pub elapsed: f32,
    pub tick_count: u32,
    pub runners: [Runner; 2],
    pub ghosts: Vec<Ghost>,
    pub pellets: PelletField,
    pub phase: MatchPhase,
    rng: Rng,
    next_power_at: f32,
}

impl Sim {
    pub fn new(seed: u64) -> Sim {
        let opening = Difficulty::at(0.0);
        let ghosts = (0..opening.ghost_count).map(Ghost::new).collect();

        Sim {
            elapsed: 0.0,
            tick_count: 0,
            runners: [Runner::new(0), Runner::new(1)],
            ghosts,
            pellets: PelletField::new_full(),
            phase: MatchPhase::Waiting,
            rng: Rng::seeded(seed),
            next_power_at: opening.power_pellet_interval,
        }
    }

    pub fn start(&mut self) {
        if self.phase == MatchPhase::Waiting {
            self.phase = MatchPhase::Running;
        }
    }

    pub fn is_running(&self) -> bool {
        self.phase == MatchPhase::Running
    }

    pub fn difficulty(&self) -> Difficulty {
        Difficulty::at(self.elapsed)
    }

    /// Queues a turn for a slot. Applied on the next tick.
    pub fn set_input(&mut self, slot: u8, dir: Dir) {
        if let Some(p) = self.runners.get_mut(slot as usize) {
            p.desired = Some(dir);
        }
    }

    /// Removes a player from the match: they never connected, or they left.
    /// Their remaining lives are forfeit.
    pub fn abandon(&mut self, slot: u8) {
        if let Some(p) = self.runners.get_mut(slot as usize) {
            if p.state != RunnerState::Out {
                p.lives = 0;
                p.state = RunnerState::Out;
            }
        }
        self.update_phase();
    }

    pub fn winner(&self) -> Option<u8> {
        match self.runners[0].score.cmp(&self.runners[1].score) {
            std::cmp::Ordering::Greater => Some(0),
            std::cmp::Ordering::Less => Some(1),
            std::cmp::Ordering::Equal => None,
        }
    }

    /// One fixed 30Hz step.
    ///
    /// The order below is load bearing: it decides who wins a tie when a
    /// a runner and a ghost reach the same tile on the same tick, and it makes
    /// sure a pellet that just dripped cannot be eaten before it is visible.
    pub fn tick(&mut self) -> TickEvents {
        let mut events = TickEvents::default();
        if self.phase != MatchPhase::Running {
            return events;
        }

        // 1. clock and difficulty
        self.elapsed += TICK_DT;
        self.tick_count += 1;
        let diff = Difficulty::at(self.elapsed);
        while self.ghosts.len() < diff.ghost_count {
            self.ghosts.push(Ghost::new(self.ghosts.len()));
        }

        // 2. respawns, then runner movement
        self.resolve_respawns();
        self.move_runners();

        // 3. pellet pickups
        self.eat_pellets(&mut events, &diff);

        // 4. ghosts
        self.move_ghosts(&diff);

        // 5. runners against ghosts
        self.resolve_ghost_contacts(&mut events);

        // 6. runner against runner
        self.resolve_pvp(&mut events);

        // 7. board upkeep
        self.replenish(&mut events, &diff);

        // 8. is it over
        self.update_phase();

        events
    }

    fn resolve_respawns(&mut self) {
        let now = self.elapsed;
        for p in self.runners.iter_mut() {
            if p.state == RunnerState::Dying && now >= p.respawn_at {
                p.pos = GridPos::new(
                    p.spawn_tile(),
                    if p.slot == 0 { Dir::Right } else { Dir::Left },
                );
                p.desired = None;
                p.state = RunnerState::Alive;
                p.ghost_combo = 0;
            }
        }
    }

    fn move_runners(&mut self) {
        let now = self.elapsed;
        for p in self.runners.iter_mut() {
            if !p.can_move(now) {
                continue;
            }
            p.pos
                .advance(p.desired, Mover::Runner, RUNNER_SPEED, TICK_DT);
        }
    }

    fn eat_pellets(&mut self, events: &mut TickEvents, diff: &Difficulty) {
        let now = self.elapsed;
        let mut energized_someone = false;

        for p in self.runners.iter_mut() {
            if !p.is_active() {
                continue;
            }
            let tile = p.pos.occupied_tile();
            let Some(kind) = self.pellets.take(tile) else {
                continue;
            };

            match kind {
                PelletKind::Normal => p.score += score::PELLET,
                PelletKind::Power => {
                    p.score += score::POWER_PELLET;
                    p.energized_until = now + diff.energized_duration;
                    p.ghost_combo = 0;
                    energized_someone = true;
                }
            }
            events.pellets_eaten.push((tile, kind, p.slot));
        }

        if energized_someone {
            for g in self.ghosts.iter_mut() {
                g.frighten();
            }
        }
    }

    fn move_ghosts(&mut self, diff: &Difficulty) {
        let now = self.elapsed;
        let anyone_energized = self.runners.iter().any(|p| p.is_energized(now));

        let targets: Vec<RunnerTarget> = self
            .runners
            .iter()
            .map(|p| RunnerTarget {
                tile: p.pos.tile,
                dir: p.pos.dir,
                alive: p.is_active(),
            })
            .collect();
        let red_tile = self.ghosts.first().map(|g| g.pos.tile).unwrap_or(IVec2::ZERO);

        for g in self.ghosts.iter_mut() {
            if !anyone_energized {
                g.unfrighten();
            }
            g.tick_mode(TICK_DT);
            let target = g.target(&targets, red_tile);
            let speed = diff.ghost_speed() * g.speed_multiplier();
            g.step(target, speed, TICK_DT);
        }
    }

    fn resolve_ghost_contacts(&mut self, events: &mut TickEvents) {
        let now = self.elapsed;
        let mut killed: Vec<u8> = Vec::new();

        for slot in 0..2usize {
            if !self.runners[slot].is_active() {
                continue;
            }
            let runner_pos = self.runners[slot].pos.world();
            let energized = self.runners[slot].is_energized(now);

            for (gi, g) in self.ghosts.iter_mut().enumerate() {
                if !touching(runner_pos, g.pos.world()) {
                    continue;
                }
                if energized && g.is_edible() {
                    let p = &mut self.runners[slot];
                    let points = score::ghost_points(p.ghost_combo);
                    p.ghost_combo += 1;
                    p.score += points;
                    g.eat();
                    events.ghosts_eaten.push((gi, slot as u8, points));
                } else if g.is_dangerous() {
                    killed.push(slot as u8);
                    break;
                }
            }
        }

        for slot in killed {
            self.kill(slot, events);
        }
    }

    fn resolve_pvp(&mut self, events: &mut TickEvents) {
        let now = self.elapsed;
        let (a, b) = (&self.runners[0], &self.runners[1]);
        if !a.is_active() || !b.is_active() {
            return;
        }
        if !touching(a.pos.world(), b.pos.world()) {
            return;
        }

        let a_hunting = a.is_energized(now);
        let b_hunting = b.is_energized(now);

        // Two energized runners bounce off each other. Neither is prey.
        let thief = match (a_hunting, b_hunting) {
            (true, false) => 0usize,
            (false, true) => 1usize,
            _ => return,
        };
        let victim = 1 - thief;

        if self.runners[victim].is_immune(now) {
            return;
        }

        let nominal = score::steal_amount(self.runners[victim].score);
        let taken = nominal.min(self.runners[victim].score);

        self.runners[victim].score -= taken;
        self.runners[victim].stunned_until = now + score::STUN_SECS;
        self.runners[victim].immune_until = now + score::STEAL_IMMUNITY_SECS;
        self.runners[thief].score += taken;

        events
            .steals
            .push((thief as u8, victim as u8, taken));
    }

    fn replenish(&mut self, events: &mut TickEvents, diff: &Difficulty) {
        let live: Vec<IVec2> = self
            .runners
            .iter()
            .filter(|p| p.is_active())
            .map(|p| p.pos.tile)
            .collect();

        events
            .pellets_spawned
            .extend(self.pellets.drip(&mut self.rng, &live, TICK_DT));

        if self.elapsed >= self.next_power_at {
            self.next_power_at = self.elapsed + diff.power_pellet_interval;
            if let Some(tile) = self.pellets.spawn_power(&mut self.rng, &live) {
                events.pellets_spawned.push((tile, PelletKind::Power));
            }
        }
    }

    fn kill(&mut self, slot: u8, events: &mut TickEvents) {
        let now = self.elapsed;
        let p = &mut self.runners[slot as usize];
        if !p.is_active() {
            return;
        }

        p.lives = p.lives.saturating_sub(1);
        p.energized_until = 0.0;
        p.ghost_combo = 0;
        events.deaths.push(slot);

        if p.lives == 0 {
            p.state = RunnerState::Out;
            events.eliminated.push(slot);
        } else {
            p.state = RunnerState::Dying;
            p.respawn_at = now + score::DEATH_FREEZE_SECS;
        }

        // Give the board some air after a death. Ghosts that are already eyes
        // keep going home; the rest back off to their corners.
        for g in self.ghosts.iter_mut() {
            if g.mode != GhostMode::Eaten {
                g.mode = GhostMode::Scatter;
                g.mode_timer = score::SCATTER_AFTER_DEATH_SECS;
            }
        }
    }

    fn update_phase(&mut self) {
        if matches!(self.phase, MatchPhase::Finished { .. } | MatchPhase::Waiting) {
            return;
        }
        if self.runners.iter().all(|p| p.state == RunnerState::Out) {
            self.phase = MatchPhase::Finished {
                winner: self.winner(),
            };
        }
    }
}

/// Distance test that respects the tunnel wrap, so two things either side of
/// the seam are correctly seen as adjacent.
fn touching(a: Vec2, b: Vec2) -> bool {
    let w = MAZE_W as f32;
    let raw_dx = (a.x - b.x).abs();
    let dx = raw_dx.min(w - raw_dx);
    let dy = a.y - b.y;
    (dx * dx + dy * dy) < score::COLLIDE_RADIUS * score::COLLIDE_RADIUS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ghosts::GhostMode;

    fn running_sim() -> Sim {
        let mut s = Sim::new(1234);
        s.start();
        s
    }

    /// Drops every ghost far away and asleep so a test can isolate one rule.
    fn park_ghosts(sim: &mut Sim) {
        for g in sim.ghosts.iter_mut() {
            g.pos = GridPos::new(crate::maze::GHOST_HOUSE, Dir::Up);
            g.mode = GhostMode::Eaten;
            g.penned = true;
        }
    }

    #[test]
    fn a_new_sim_waits_before_running() {
        let mut s = Sim::new(1);
        assert_eq!(s.phase, MatchPhase::Waiting);
        s.tick();
        assert_eq!(s.elapsed, 0.0, "a waiting match must not advance");
        s.start();
        s.tick();
        assert!(s.elapsed > 0.0);
    }

    #[test]
    fn both_players_start_with_three_lives_and_no_score() {
        let s = Sim::new(1);
        for p in &s.runners {
            assert_eq!(p.lives, 3);
            assert_eq!(p.score, 0);
            assert_eq!(p.state, RunnerState::Alive);
        }
    }

    #[test]
    fn eating_a_pellet_is_worth_ten() {
        let mut s = running_sim();
        park_ghosts(&mut s);
        // Clear the board, then plant one pellet right where slot 0 will land.
        s.pellets = PelletField::empty();
        let target = s.runners[0].pos.occupied_tile();
        s.pellets.put(target, PelletKind::Normal);

        let ev = s.tick();
        assert_eq!(s.runners[0].score, score::PELLET);
        assert_eq!(ev.pellets_eaten.len(), 1);
    }

    #[test]
    fn a_power_pellet_energizes_and_frightens_every_ghost() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        let target = s.runners[0].pos.occupied_tile();
        s.pellets.put(target, PelletKind::Power);

        s.tick();
        assert!(s.runners[0].is_energized(s.elapsed));
        assert_eq!(s.runners[0].score, score::POWER_PELLET);
        assert!(s.ghosts.iter().all(|g| g.mode == GhostMode::Frightened));
    }

    #[test]
    fn the_ghost_chain_doubles_within_one_energization() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        s.runners[0].energized_until = 100.0;
        for g in s.ghosts.iter_mut() {
            g.frighten();
        }

        let mut awarded = Vec::new();
        for i in 0..4 {
            // Park a frightened ghost on top of slot 0 and let contact resolve.
            s.ghosts[i].pos = s.runners[0].pos;
            s.ghosts[i].mode = GhostMode::Frightened;
            let ev = s.tick();
            awarded.extend(ev.ghosts_eaten.iter().map(|(_, _, pts)| *pts));
        }

        assert_eq!(awarded, vec![200, 400, 800, 1600]);
    }

    #[test]
    fn a_dangerous_ghost_takes_a_life() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        s.ghosts[0].pos = s.runners[0].pos;
        s.ghosts[0].mode = GhostMode::Chase;
        s.ghosts[0].penned = false;

        let ev = s.tick();
        assert_eq!(ev.deaths, vec![0]);
        assert_eq!(s.runners[0].lives, 2);
        assert_eq!(s.runners[0].state, RunnerState::Dying);
    }

    #[test]
    fn a_dying_player_respawns_at_their_own_corner() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        park_ghosts(&mut s);
        s.runners[1].state = RunnerState::Dying;
        s.runners[1].respawn_at = s.elapsed + 0.001;
        s.runners[1].pos = GridPos::new(IVec2::new(1, 1), Dir::Up);

        s.tick();
        assert_eq!(s.runners[1].state, RunnerState::Alive);
        assert_eq!(s.runners[1].pos.tile, SPAWN_P1);
    }

    #[test]
    fn losing_the_last_life_puts_you_out_but_the_match_runs_on() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        s.runners[0].lives = 1;
        s.ghosts[0].pos = s.runners[0].pos;
        s.ghosts[0].mode = GhostMode::Chase;
        s.ghosts[0].penned = false;

        let ev = s.tick();
        assert_eq!(ev.eliminated, vec![0]);
        assert_eq!(s.runners[0].state, RunnerState::Out);
        assert_eq!(s.phase, MatchPhase::Running, "slot 1 is still playing");
    }

    #[test]
    fn the_match_finishes_only_when_both_players_are_out() {
        let mut s = running_sim();
        s.runners[0].score = 500;
        s.runners[1].score = 100;

        s.abandon(0);
        assert_eq!(s.phase, MatchPhase::Running);

        s.abandon(1);
        assert_eq!(s.phase, MatchPhase::Finished { winner: Some(0) });
    }

    #[test]
    fn the_winner_is_whoever_scored_more() {
        let mut s = running_sim();
        s.runners[0].score = 10;
        s.runners[1].score = 20;
        s.abandon(0);
        s.abandon(1);
        assert_eq!(s.phase, MatchPhase::Finished { winner: Some(1) });
    }

    #[test]
    fn an_equal_score_is_a_draw() {
        let mut s = running_sim();
        s.runners[0].score = 700;
        s.runners[1].score = 700;
        s.abandon(0);
        s.abandon(1);
        assert_eq!(s.phase, MatchPhase::Finished { winner: None });
    }

    #[test]
    fn ghosts_ignore_a_player_who_is_out() {
        let mut s = running_sim();
        s.abandon(0);
        let targets: Vec<RunnerTarget> = s
            .runners
            .iter()
            .map(|p| RunnerTarget {
                tile: p.pos.tile,
                dir: p.pos.dir,
                alive: p.is_active(),
            })
            .collect();

        let mut g = Ghost::new(0);
        g.penned = false;
        g.mode = GhostMode::Chase;
        g.pos = GridPos::new(SPAWN_P0, Dir::Up);
        assert_eq!(
            g.target(&targets, IVec2::ZERO),
            s.runners[1].pos.tile,
            "every ghost should converge on the survivor"
        );
    }

    #[test]
    fn an_energized_player_robs_a_tenth_and_stuns_the_rival() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        park_ghosts(&mut s);
        s.runners[0].energized_until = 100.0;
        s.runners[1].score = 2000;
        s.runners[1].pos = s.runners[0].pos;

        let ev = s.tick();
        assert_eq!(ev.steals.len(), 1, "expected exactly one steal");
        let (thief, victim, amount) = ev.steals[0];
        assert_eq!((thief, victim, amount), (0, 1, 200));
        assert_eq!(s.runners[1].score, 1800);
        assert_eq!(s.runners[0].score, 200);
        assert!(s.runners[1].is_stunned(s.elapsed));
    }

    #[test]
    fn a_steal_never_costs_a_life() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        park_ghosts(&mut s);
        s.runners[0].energized_until = 100.0;
        s.runners[1].score = 1000;
        s.runners[1].pos = s.runners[0].pos;

        s.tick();
        assert_eq!(s.runners[1].lives, 3, "eating your rival must not cost a life");
        assert_eq!(s.runners[1].state, RunnerState::Alive);
    }

    #[test]
    fn a_robbed_player_is_immune_for_five_seconds() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        park_ghosts(&mut s);
        s.runners[0].energized_until = 1000.0;
        s.runners[1].score = 5000;
        s.runners[1].pos = s.runners[0].pos;

        let first = s.tick();
        assert_eq!(first.steals.len(), 1);

        // Keep them overlapping. Immunity must hold for longer than the stun.
        let mut more = 0;
        for _ in 0..(4.0 / TICK_DT) as usize {
            s.runners[1].pos = s.runners[0].pos;
            more += s.tick().steals.len();
        }
        assert_eq!(more, 0, "a second steal landed inside the immunity window");
    }

    #[test]
    fn two_energized_players_cannot_rob_each_other() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        park_ghosts(&mut s);
        s.runners[0].energized_until = 100.0;
        s.runners[1].energized_until = 100.0;
        s.runners[1].score = 1000;
        s.runners[1].pos = s.runners[0].pos;

        assert!(s.tick().steals.is_empty());
    }

    #[test]
    fn a_stunned_player_cannot_move() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        park_ghosts(&mut s);
        s.runners[1].stunned_until = s.elapsed + 10.0;
        let before = s.runners[1].pos;

        for _ in 0..10 {
            s.tick();
        }
        assert_eq!(s.runners[1].pos, before);
    }

    #[test]
    fn ghost_count_grows_with_the_difficulty_curve() {
        let mut s = running_sim();
        assert_eq!(s.ghosts.len(), 4);

        // Step 5 (2:30) is still six ghosts.
        s.elapsed = 155.0;
        s.tick();
        assert_eq!(s.ghosts.len(), 6);

        // Step 6 (3:00) brings the seventh.
        s.elapsed = 185.0;
        s.tick();
        assert_eq!(s.ghosts.len(), Difficulty::at(s.elapsed).ghost_count);
        assert_eq!(s.ghosts.len(), 7);
    }

    #[test]
    fn power_pellets_appear_on_schedule() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        park_ghosts(&mut s);

        let mut powers = 0;
        for _ in 0..(25.0 / TICK_DT) as usize {
            powers += s
                .tick()
                .pellets_spawned
                .iter()
                .filter(|(_, k)| *k == PelletKind::Power)
                .count();
        }
        assert!(powers >= 1, "no power pellet in the first 25 seconds");
    }

    #[test]
    fn the_board_never_runs_dry() {
        let mut s = running_sim();
        s.pellets = PelletField::empty();
        park_ghosts(&mut s);

        for _ in 0..(10.0 / TICK_DT) as usize {
            s.tick();
        }
        assert!(s.pellets.count() > 0, "the drip failed to refill the maze");
    }

    #[test]
    fn abandoning_forfeits_the_remaining_lives() {
        let mut s = running_sim();
        s.abandon(1);
        assert_eq!(s.runners[1].lives, 0);
        assert_eq!(s.runners[1].state, RunnerState::Out);
    }

    #[test]
    fn abandoning_twice_is_harmless() {
        let mut s = running_sim();
        s.runners[0].score = 1;
        s.abandon(1);
        s.abandon(1);
        assert_eq!(s.phase, MatchPhase::Running);
    }

    /// The whole thing has to survive a long match without panicking, without
    /// anything leaving the maze, and without the clock stalling.
    #[test]
    fn a_five_minute_match_stays_consistent() {
        let mut s = running_sim();
        let ticks = (300.0 / TICK_DT) as usize;

        for i in 0..ticks {
            if i % 37 == 0 {
                s.set_input(0, Dir::ALL[i % 4]);
            }
            if i % 53 == 0 {
                s.set_input(1, Dir::ALL[(i + 2) % 4]);
            }
            s.tick();

            for p in &s.runners {
                assert!(
                    crate::MAZE.walkable_by_runner(p.pos.tile),
                    "slot {} left the maze at {:?}",
                    p.slot,
                    p.pos.tile
                );
                assert!(p.lives <= score::LIVES);
            }
            for g in &s.ghosts {
                assert!(crate::MAZE.walkable_by_ghost(g.pos.tile));
            }

            if !s.is_running() {
                break;
            }
        }

        assert!(s.elapsed > 0.0);
    }

    #[test]
    fn the_same_seed_produces_the_same_match() {
        let run = |seed: u64| {
            let mut s = Sim::new(seed);
            s.start();
            for i in 0..600 {
                s.set_input(0, Dir::ALL[i % 4]);
                s.tick();
            }
            (s.runners[0].score, s.runners[1].score, s.pellets.count())
        };
        assert_eq!(run(77), run(77));
    }
}
