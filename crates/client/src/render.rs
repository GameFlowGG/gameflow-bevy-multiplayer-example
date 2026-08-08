//! Drawing the maze.
//!
//! Walls are flat coloured sprites spawned once. Everything that moves uses the
//! art under `assets/`: Pac-Man is directional and animated, ghosts carry their
//! personality colour, pellets are a dot and power pellets a fruit. Pellets are
//! kept in sync by diffing the field against what is on screen, which is cheap
//! because only a handful of tiles change per tick.

use std::collections::HashMap;

use bevy::prelude::*;

use pacman_shared::ghosts::GhostMode;
use pacman_shared::maze::{MAZE, MAZE_H, MAZE_W};
use pacman_shared::movement::Dir;
use pacman_shared::sim::PacState;

use crate::net::Match;
use crate::Screen;

pub const TILE: f32 = 20.0;

const Z_WALL: f32 = 0.0;
const Z_PELLET: f32 = 1.0;
const Z_GHOST: f32 = 2.0;
const Z_PACMAN: f32 = 3.0;

const WALL_COLOUR: Color = Color::srgb(0.13, 0.16, 0.42);
/// Normal pellets are drawn as a solid dot rather than art: the dot image is a
/// 2px speck on a 16px canvas and disappears once scaled down to a tile.
const PELLET_COLOUR: Color = Color::srgb(1.0, 0.86, 0.55);
/// The local Pac-Man art is yellow; drawn untinted it stays yellow.
const LOCAL_TINT: Color = Color::WHITE;
/// The rival uses the same art, tinted so the two are never confused.
const RIVAL_TINT: Color = Color::srgb(0.45, 0.85, 1.0);
const ENERGIZED_TINT: Color = Color::srgb(0.6, 1.0, 1.0);
const STUNNED_TINT: Color = Color::srgb(0.45, 0.45, 0.45);

/// How long each of the three mouth frames is shown.
const ANIM_FRAME_SECS: f32 = 0.08;

/// Marks everything that belongs to the board, so leaving the match can clear
/// it in one sweep.
#[derive(Component)]
pub struct BoardEntity;

#[derive(Component)]
struct LocalPac;

#[derive(Component)]
struct RivalPac;

#[derive(Component)]
struct GhostSprite(usize);

#[derive(Resource, Default)]
struct PelletSprites(HashMap<IVec2, Entity>);

/// Every image the board draws, loaded once at startup.
#[derive(Resource)]
struct Art {
    /// Pac-Man frames indexed by direction then frame, `[dir][frame]`.
    pacman: [[Handle<Image>; 3]; 4],
    /// One image per ghost personality.
    ghosts: [Handle<Image>; 4],
    /// Shown while a ghost is frightened.
    frightened: Handle<Image>,
    /// The power pellet fruit. Normal pellets are drawn as a flat dot instead.
    power: Handle<Image>,
}

/// The shared mouth animation. Both Pac-Men chomp on the same clock.
#[derive(Resource)]
struct PacAnim {
    timer: Timer,
    frame: usize,
}

/// Maps a facing direction to its row in `Art::pacman`.
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
                (animate, sync_pellets, place_pacmen, place_ghosts)
                    .run_if(in_state(Screen::Playing)),
            );
    }
}

fn load_art(mut commands: Commands, assets: Res<AssetServer>) {
    let load_dir = |dir: &str| {
        [
            assets.load(format!("pacman/{dir}/1.png")),
            assets.load(format!("pacman/{dir}/2.png")),
            assets.load(format!("pacman/{dir}/3.png")),
        ]
    };
    commands.insert_resource(Art {
        // Same order as dir_index: up, down, left, right.
        pacman: [
            load_dir("up"),
            load_dir("down"),
            load_dir("left"),
            load_dir("right"),
        ],
        ghosts: [
            assets.load("ghosts/blinky.png"),
            assets.load("ghosts/pinky.png"),
            assets.load("ghosts/inky.png"),
            assets.load("ghosts/clyde.png"),
        ],
        frightened: assets.load("ghosts/blue_ghost.png"),
        power: assets.load("pellets/apple.png"),
    });
    commands.insert_resource(PacAnim {
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
            if MAZE.tile(x, y) != pacman_shared::Tile::Wall {
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

    let pac = |dir: Dir| sprite(art.pacman[dir_index(dir)][0].clone(), TILE * 1.1);
    commands.spawn((
        pac(Dir::Right),
        Transform::from_xyz(0.0, 0.0, Z_PACMAN),
        LocalPac,
        BoardEntity,
    ));
    commands.spawn((
        pac(Dir::Left),
        Transform::from_xyz(0.0, 0.0, Z_PACMAN),
        RivalPac,
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

fn animate(time: Res<Time>, mut anim: ResMut<PacAnim>) {
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
                    pacman_shared::PelletKind::Normal => {
                        Sprite::from_color(PELLET_COLOUR, Vec2::splat(TILE * 0.3))
                    }
                    pacman_shared::PelletKind::Power => sprite(art.power.clone(), TILE * 0.9),
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
fn place_pacmen(
    game: Option<Res<Match>>,
    art: Res<Art>,
    anim: Res<PacAnim>,
    mut local: Query<(&mut Transform, &mut Sprite), (With<LocalPac>, Without<RivalPac>)>,
    mut rival: Query<(&mut Transform, &mut Sprite), (With<RivalPac>, Without<LocalPac>)>,
) {
    let Some(game) = game else {
        return;
    };
    let slot = game.slot as usize;
    let other = game.rival_slot();

    if let Ok((mut t, mut s)) = local.single_mut() {
        let w = game.local.pos.world();
        let p = tile_to_world(w.x, w.y);
        t.translation = Vec3::new(p.x, p.y, Z_PACMAN);
        s.image = art.pacman[dir_index(game.local.pos.dir)][anim.frame].clone();
        s.color = pac_tint(LOCAL_TINT, game.states[slot], game.energized[slot], game.stunned[slot]);
    }
    if let Ok((mut t, mut s)) = rival.single_mut() {
        let w = game.remote.render();
        let p = tile_to_world(w.x, w.y);
        t.translation = Vec3::new(p.x, p.y, Z_PACMAN);
        s.image = art.pacman[dir_index(game.remote_pos.dir)][anim.frame].clone();
        s.color = pac_tint(RIVAL_TINT, game.states[other], game.energized[other], game.stunned[other]);
    }
}

/// The tint over a Pac-Man's art. Base is white for the local player (leaving the
/// yellow art alone) and a colour for the rival. State overrides it so death,
/// stun and power are readable at a glance.
fn pac_tint(base: Color, state: PacState, energized: bool, stunned: bool) -> Color {
    match state {
        // Out of the match entirely: leave a faint ghost of them behind.
        PacState::Out => base.with_alpha(0.15),
        PacState::Dying => base.with_alpha(0.35),
        PacState::Alive if stunned => STUNNED_TINT,
        PacState::Alive if energized => ENERGIZED_TINT,
        PacState::Alive => base,
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
            sprite(art.ghosts[index % 4].clone(), TILE * 1.1),
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

/// Which ghost image to show and how to tint it. Frightened swaps to the blue
/// ghost so the player can tell at a glance what can be eaten; eaten ghosts fade
/// to just their trail back home.
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
        let solid = pac_tint(LOCAL_TINT, PacState::Alive, false, false);
        let out = pac_tint(LOCAL_TINT, PacState::Out, false, false);
        assert_ne!(solid, out);
        assert!(out.alpha() < solid.alpha());
    }

    #[test]
    fn stunned_reads_differently_from_energized() {
        let stunned = pac_tint(LOCAL_TINT, PacState::Alive, false, true);
        let energized = pac_tint(LOCAL_TINT, PacState::Alive, true, false);
        assert_ne!(stunned, energized);
    }
}
