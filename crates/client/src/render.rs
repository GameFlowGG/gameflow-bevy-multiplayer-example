//! Drawing the maze.
//!
//! Walls are flat coloured sprites spawned once. Everything that moves uses the
//! art under `assets/`: the runner is directional and animated, ghosts carry
//! their personality colour and wear an expression to match it, and the power
//! pellet is an oversized dot. Pellets are kept in sync by diffing the field
//! against what is on screen, which is cheap because only a handful of tiles
//! change per tick.

use std::collections::HashMap;

use bevy::prelude::*;

use ghostchase_shared::ghosts::GhostMode;
use ghostchase_shared::maze::{MAZE, MAZE_H, MAZE_W};
use ghostchase_shared::movement::Dir;
use ghostchase_shared::sim::RunnerState;

use crate::net::Match;
use crate::Screen;

pub const TILE: f32 = 20.0;

const Z_WALL: f32 = 0.0;
const Z_PELLET: f32 = 1.0;
const Z_GHOST: f32 = 2.0;
const Z_RUNNER: f32 = 3.0;

const WALL_COLOUR: Color = Color::srgb(0.13, 0.16, 0.42);
/// Normal pellets are drawn as a solid dot rather than art: the sheet's dot is
/// 6px of a 16px canvas, so it all but vanishes at the size a pellet renders.
/// The colour is the sheet's own pellet peach, so drawn and loaded art match.
const PELLET_COLOUR: Color = Color::srgb_u8(0xff, 0xb5, 0x70);
/// The local runner art is peach; drawn untinted it stays peach.
const LOCAL_TINT: Color = Color::WHITE;
/// The rival uses the same art, tinted so the two are never confused. A tint
/// multiplies, so it can only ever darken: the peach art has little green and
/// less blue, which rules out tinting the rival cool. Killing the red instead
/// lands on a saturated green that reads clearly against the local peach.
const RIVAL_TINT: Color = Color::srgb(0.25, 1.0, 1.0);
const ENERGIZED_TINT: Color = Color::srgb(0.6, 1.0, 1.0);
const STUNNED_TINT: Color = Color::srgb(0.45, 0.45, 0.45);

/// How long each of the three mouth frames is shown.
const ANIM_FRAME_SECS: f32 = 0.08;

/// Everything that moves is drawn at the art's native 32px, so one source pixel
/// is one screen pixel and nearest sampling has nothing left to resample. The
/// sprites carry their own padding rather than filling the canvas — the runner
/// occupies 22 of those 32 pixels, a ghost 20 — which is what holds every
/// animation frame on a common origin, and what keeps the two in proportion.
const SPRITE_SIZE: f32 = TILE * 1.6;
/// Normal pellets, drawn flat. A third of a tile reads as a dot without ever
/// being mistaken for the power pellet.
const PELLET_SIZE: f32 = TILE * 0.3;
/// The power pellet art is a 10px dot on a 16px canvas, so it is drawn larger
/// than the tile to land at roughly the size the sprite itself suggests.
const POWER_PELLET_SIZE: f32 = TILE * 1.15;

/// Marks everything that belongs to the board, so leaving the match can clear
/// it in one sweep.
#[derive(Component)]
pub struct BoardEntity;

#[derive(Component)]
struct LocalRunner;

#[derive(Component)]
struct RivalRunner;

#[derive(Component)]
struct GhostSprite(usize);

#[derive(Resource, Default)]
struct PelletSprites(HashMap<IVec2, Entity>);

/// Every image the board draws, loaded once at startup.
#[derive(Resource)]
struct Art {
    /// Runner frames indexed by direction then frame, `[dir][frame]`.
    runner: [[Handle<Image>; 3]; 4],
    /// One image per ghost personality, in `Personality::for_slot` order.
    ghosts: [Handle<Image>; 4],
    /// Shown while a ghost is frightened.
    frightened: Handle<Image>,
    /// The power pellet. Normal pellets are drawn as a flat dot instead.
    power: Handle<Image>,
}

/// The shared mouth animation. Both runners chomp on the same clock.
#[derive(Resource)]
struct RunnerAnim {
    timer: Timer,
    frame: usize,
}

/// Maps a facing direction to its row in `Art::runner`.
fn dir_index(dir: Dir) -> usize {
    match dir {
        Dir::Up => 0,
        Dir::Down => 1,
        Dir::Left => 2,
        Dir::Right => 3,
    }
}

pub struct RenderPlugin;

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PelletSprites>()
            .add_systems(Startup, load_art)
            .add_systems(OnEnter(Screen::Playing), spawn_board)
            .add_systems(OnExit(Screen::Result), clear_board)
            .add_systems(
                Update,
                (animate, sync_pellets, place_runners, place_ghosts)
                    .run_if(in_state(Screen::Playing)),
            );
    }
}

fn load_art(mut commands: Commands, assets: Res<AssetServer>) {
    let load_dir = |dir: &str| {
        [
            assets.load(format!("runner/{dir}/1.png")),
            assets.load(format!("runner/{dir}/2.png")),
            assets.load(format!("runner/{dir}/3.png")),
        ]
    };
    commands.insert_resource(Art {
        // Same order as dir_index: up, down, left, right.
        runner: [
            load_dir("up"),
            load_dir("down"),
            load_dir("left"),
            load_dir("right"),
        ],
        // Named for the personality they belong to, not for a character.
        ghosts: [
            assets.load("ghosts/red.png"),
            assets.load("ghosts/pink.png"),
            assets.load("ghosts/cyan.png"),
            assets.load("ghosts/orange.png"),
        ],
        frightened: assets.load("ghosts/frightened.png"),
        power: assets.load("pellets/power.png"),
    });
    commands.insert_resource(RunnerAnim {
        timer: Timer::from_seconds(ANIM_FRAME_SECS, TimerMode::Repeating),
        frame: 0,
    });
}

/// Tile coordinates to world pixels. The maze is centred on the origin and y is
/// flipped, because the grid counts downward and the screen counts up.
pub fn tile_to_world(x: f32, y: f32) -> Vec2 {
    Vec2::new(
        (x - MAZE_W as f32 / 2.0 + 0.5) * TILE,
        -(y - MAZE_H as f32 / 2.0 + 0.5) * TILE,
    )
}

fn sprite(image: Handle<Image>, size: f32) -> Sprite {
    Sprite {
        custom_size: Some(Vec2::splat(size)),
        ..Sprite::from_image(image)
    }
}

fn spawn_board(mut commands: Commands, art: Res<Art>, mut sprites: ResMut<PelletSprites>) {
    for y in 0..MAZE_H as i32 {
        for x in 0..MAZE_W as i32 {
            if MAZE.tile(x, y) != ghostchase_shared::Tile::Wall {
                continue;
            }
            let p = tile_to_world(x as f32, y as f32);
            commands.spawn((
                Sprite::from_color(WALL_COLOUR, Vec2::splat(TILE)),
                Transform::from_xyz(p.x, p.y, Z_WALL),
                BoardEntity,
            ));
        }
    }

    let runner = |dir: Dir| sprite(art.runner[dir_index(dir)][0].clone(), SPRITE_SIZE);
    commands.spawn((
        runner(Dir::Right),
        Transform::from_xyz(0.0, 0.0, Z_RUNNER),
        LocalRunner,
        BoardEntity,
    ));
    commands.spawn((
        runner(Dir::Left),
        Transform::from_xyz(0.0, 0.0, Z_RUNNER),
        RivalRunner,
        BoardEntity,
    ));

    sprites.0.clear();
}

fn clear_board(
    mut commands: Commands,
    board: Query<Entity, With<BoardEntity>>,
    mut sprites: ResMut<PelletSprites>,
) {
    for entity in board.iter() {
        commands.entity(entity).despawn();
    }
    sprites.0.clear();
}

fn animate(time: Res<Time>, mut anim: ResMut<RunnerAnim>) {
    anim.timer.tick(time.delta());
    if anim.timer.just_finished() {
        anim.frame = (anim.frame + 1) % 3;
    }
}

fn sync_pellets(
    mut commands: Commands,
    game: Option<Res<Match>>,
    art: Res<Art>,
    mut sprites: ResMut<PelletSprites>,
) {
    let Some(game) = game else {
        return;
    };

    for tile in MAZE.corridors() {
        match game.pellets.at(tile) {
            Some(kind) => {
                if sprites.0.contains_key(&tile) {
                    continue;
                }
                let p = tile_to_world(tile.x as f32, tile.y as f32);
                let pellet = match kind {
                    ghostchase_shared::PelletKind::Normal => {
                        Sprite::from_color(PELLET_COLOUR, Vec2::splat(PELLET_SIZE))
                    }
                    ghostchase_shared::PelletKind::Power => sprite(art.power.clone(), POWER_PELLET_SIZE),
                };
                let entity = commands
                    .spawn((
                        pellet,
                        Transform::from_xyz(p.x, p.y, Z_PELLET),
                        BoardEntity,
                    ))
                    .id();
                sprites.0.insert(tile, entity);
            }
            None => {
                if let Some(entity) = sprites.0.remove(&tile) {
                    commands.entity(entity).despawn();
                }
            }
        }
    }
}

#[allow(clippy::type_complexity)]
fn place_runners(
    game: Option<Res<Match>>,
    art: Res<Art>,
    anim: Res<RunnerAnim>,
    mut local: Query<(&mut Transform, &mut Sprite), (With<LocalRunner>, Without<RivalRunner>)>,
    mut rival: Query<(&mut Transform, &mut Sprite), (With<RivalRunner>, Without<LocalRunner>)>,
) {
    let Some(game) = game else {
        return;
    };
    let slot = game.slot as usize;
    let other = game.rival_slot();

    if let Ok((mut t, mut s)) = local.single_mut() {
        let w = game.local.pos.world();
        let p = tile_to_world(w.x, w.y);
        t.translation = Vec3::new(p.x, p.y, Z_RUNNER);
        s.image = art.runner[dir_index(game.local.pos.dir)][anim.frame].clone();
        s.color = runner_tint(LOCAL_TINT, game.states[slot], game.energized[slot], game.stunned[slot]);
    }
    if let Ok((mut t, mut s)) = rival.single_mut() {
        let w = game.remote.render();
        let p = tile_to_world(w.x, w.y);
        t.translation = Vec3::new(p.x, p.y, Z_RUNNER);
        s.image = art.runner[dir_index(game.remote_pos.dir)][anim.frame].clone();
        s.color = runner_tint(RIVAL_TINT, game.states[other], game.energized[other], game.stunned[other]);
    }
}

/// The tint over a runner's art. Base is white for the local player (leaving the
/// yellow art alone) and a colour for the rival. State overrides it so death,
/// stun and power are readable at a glance.
fn runner_tint(base: Color, state: RunnerState, energized: bool, stunned: bool) -> Color {
    match state {
        // Out of the match entirely: leave a faint ghost of them behind.
        RunnerState::Out => base.with_alpha(0.15),
        RunnerState::Dying => base.with_alpha(0.35),
        RunnerState::Alive if stunned => STUNNED_TINT,
        RunnerState::Alive if energized => ENERGIZED_TINT,
        RunnerState::Alive => base,
    }
}

fn place_ghosts(
    mut commands: Commands,
    game: Option<Res<Match>>,
    art: Res<Art>,
    mut ghosts: Query<(&GhostSprite, &mut Transform, &mut Sprite)>,
) {
    let Some(game) = game else {
        return;
    };

    let existing = ghosts.iter().count();
    for index in existing..game.ghosts.len() {
        commands.spawn((
            sprite(art.ghosts[index % 4].clone(), SPRITE_SIZE),
            Transform::from_xyz(0.0, 0.0, Z_GHOST),
            GhostSprite(index),
            BoardEntity,
        ));
    }

    for (marker, mut transform, mut s) in ghosts.iter_mut() {
        let Some((interp, mode, _)) = game.ghosts.get(marker.0) else {
            continue;
        };
        let w = interp.render();
        let p = tile_to_world(w.x, w.y);
        transform.translation = Vec3::new(p.x, p.y, Z_GHOST);
        let (image, color) = ghost_look(&art, marker.0, *mode);
        s.image = image;
        s.color = color;
    }
}

/// Which ghost image to show and how to tint it. Frightened swaps to the scared
/// sprite so the player can tell at a glance what can be eaten; eaten ghosts
/// fade to just their trail back home.
fn ghost_look(art: &Art, index: usize, mode: GhostMode) -> (Handle<Image>, Color) {
    match mode {
        GhostMode::Frightened => (art.frightened.clone(), Color::WHITE),
        GhostMode::Eaten => (art.frightened.clone(), Color::WHITE.with_alpha(0.3)),
        _ => (art.ghosts[index % 4].clone(), Color::WHITE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_maze_is_centred_on_the_origin() {
        let centre = tile_to_world(MAZE_W as f32 / 2.0 - 0.5, MAZE_H as f32 / 2.0 - 0.5);
        assert!(centre.length() < 0.001, "centre landed at {centre:?}");
    }

    #[test]
    fn the_y_axis_is_flipped_for_the_screen() {
        let top = tile_to_world(14.0, 1.0);
        let bottom = tile_to_world(14.0, 29.0);
        assert!(top.y > bottom.y, "row 1 must draw above row 29");
    }

    #[test]
    fn adjacent_tiles_are_one_tile_apart() {
        let a = tile_to_world(5.0, 5.0);
        let b = tile_to_world(6.0, 5.0);
        assert!((b.x - a.x - TILE).abs() < 1e-4);
    }

    #[test]
    fn every_direction_has_its_own_row() {
        let rows: std::collections::HashSet<usize> =
            [Dir::Up, Dir::Down, Dir::Left, Dir::Right]
                .into_iter()
                .map(dir_index)
                .collect();
        assert_eq!(rows.len(), 4, "two directions collided onto one row");
    }

    #[test]
    fn a_player_who_is_out_is_drawn_faded() {
        let solid = runner_tint(LOCAL_TINT, RunnerState::Alive, false, false);
        let out = runner_tint(LOCAL_TINT, RunnerState::Out, false, false);
        assert_ne!(solid, out);
        assert!(out.alpha() < solid.alpha());
    }

    #[test]
    fn stunned_reads_differently_from_energized() {
        let stunned = runner_tint(LOCAL_TINT, RunnerState::Alive, false, true);
        let energized = runner_tint(LOCAL_TINT, RunnerState::Alive, true, false);
        assert_ne!(stunned, energized);
    }
}
