//! The difficulty curve.
//!
//! One step every 30 seconds. The curve is deliberately lethal: from step 3
//! onward ghosts are faster than Pac-Man, so an endless mode still ends. That
//! bounds a match to roughly four or five minutes, which is what keeps a
//! dedicated server from being held forever and keeps the queue moving.

/// Pac-Man never speeds up or slows down.
pub const PACMAN_SPEED: f32 = 8.0;
/// Ghost speed before the difficulty multiplier.
pub const GHOST_BASE_SPEED: f32 = 6.5;
/// Frightened ghosts crawl.
pub const GHOST_FRIGHTENED_MULT: f32 = 0.6;
/// Eaten ghosts rush back to the house.
pub const GHOST_EATEN_MULT: f32 = 2.5;

pub const STEP_SECONDS: f32 = 30.0;
const MAX_TABLE_STEP: u32 = 6;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Difficulty {
    pub step: u32,
    pub ghost_count: usize,
    pub ghost_speed_mult: f32,
    pub power_pellet_interval: f32,
    pub energized_duration: f32,
}

impl Difficulty {
    pub fn at(elapsed_secs: f32) -> Difficulty {
        let step = (elapsed_secs.max(0.0) / STEP_SECONDS) as u32;
        let t = step.min(MAX_TABLE_STEP);

        let ghost_count = match t {
            0 | 1 => 4,
            2 | 3 => 5,
            4 | 5 => 6,
            _ => 7,
        };

        // Past the table the multiplier and the interval keep growing, and the
        // energized window sits on its floor.
        let ghost_speed_mult = 1.0 + 0.10 * step as f32;
        let power_pellet_interval = 20.0 + 5.0 * step as f32;
        let energized_duration = (8.0 - step as f32).max(2.0);

        Difficulty {
            step,
            ghost_count,
            ghost_speed_mult,
            power_pellet_interval,
            energized_duration,
        }
    }

    pub fn ghost_speed(&self) -> f32 {
        GHOST_BASE_SPEED * self.ghost_speed_mult
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn the_table_matches_the_spec() {
        let cases = [
            //  secs, ghosts, mult, interval, energized
            (0.0, 4, 1.00, 20.0, 8.0),
            (30.0, 4, 1.10, 25.0, 7.0),
            (60.0, 5, 1.20, 30.0, 6.0),
            (90.0, 5, 1.30, 35.0, 5.0),
            (120.0, 6, 1.40, 40.0, 4.0),
            (150.0, 6, 1.50, 45.0, 3.0),
            (180.0, 7, 1.60, 50.0, 2.0),
        ];
        for (secs, ghosts, mult, interval, energized) in cases {
            let d = Difficulty::at(secs);
            assert_eq!(d.ghost_count, ghosts, "ghost count at {secs}s");
            assert!(close(d.ghost_speed_mult, mult), "mult at {secs}s: {d:?}");
            assert!(close(d.power_pellet_interval, interval), "interval at {secs}s");
            assert!(close(d.energized_duration, energized), "energized at {secs}s");
        }
    }

    #[test]
    fn a_step_lasts_thirty_seconds() {
        assert_eq!(Difficulty::at(29.9).step, 0);
        assert_eq!(Difficulty::at(30.0).step, 1);
        assert_eq!(Difficulty::at(59.9).step, 1);
    }

    #[test]
    fn ghosts_outrun_pacman_from_step_three_on() {
        assert!(Difficulty::at(0.0).ghost_speed() < PACMAN_SPEED);
        assert!(Difficulty::at(60.0).ghost_speed() < PACMAN_SPEED);
        assert!(
            Difficulty::at(90.0).ghost_speed() > PACMAN_SPEED,
            "the mode must turn lethal at 1:30"
        );
    }

    #[test]
    fn ghost_count_never_exceeds_seven() {
        for step in 0..40 {
            let d = Difficulty::at(step as f32 * STEP_SECONDS);
            assert!(d.ghost_count <= 7, "step {step} wanted {}", d.ghost_count);
        }
    }

    #[test]
    fn energized_duration_floors_at_two_seconds() {
        assert!(close(Difficulty::at(600.0).energized_duration, 2.0));
        assert!(close(Difficulty::at(6000.0).energized_duration, 2.0));
    }

    #[test]
    fn difficulty_never_goes_backwards() {
        let mut prev = Difficulty::at(0.0);
        for i in 1..60 {
            let cur = Difficulty::at(i as f32 * 15.0);
            assert!(cur.ghost_speed_mult >= prev.ghost_speed_mult);
            assert!(cur.ghost_count >= prev.ghost_count);
            assert!(cur.energized_duration <= prev.energized_duration);
            prev = cur;
        }
    }

    #[test]
    fn negative_time_is_treated_as_zero() {
        assert_eq!(Difficulty::at(-5.0).step, 0);
    }
}
