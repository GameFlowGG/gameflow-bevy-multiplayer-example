//! Screens and the flow between them.
//!
//! Nick, then menu, then queue, then the match, then the result. There are no
//! lobbies by design: the shortest path from opening the game to playing is one
//! key press.

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::input::ButtonState;
use bevy::prelude::*;

use ghostchase_shared::difficulty::Difficulty;
use ghostchase_shared::sim::RunnerState;

use crate::backend::{Backend, Reply, Request, Session};
use crate::identity;
use crate::net::{connect, disconnect, Assignment, Match};
use crate::Screen;

const NICK_MAX: usize = 16;
const POLL_EVERY: f32 = 1.0;

const FG: Color = Color::srgb(0.96, 0.96, 0.98);
const DIM: Color = Color::srgb(0.55, 0.57, 0.66);
const ACCENT: Color = Color::srgb(1.0, 0.85, 0.20);
const BAD: Color = Color::srgb(1.0, 0.45, 0.42);

/// The rating ordinal (`mu - 3*sigma`) starts near zero and dips below it after a
/// loss, which reads badly to a player. We present it as MMR-style points on a
/// friendly baseline instead: `points = RANK_BASELINE + ordinal * RANK_SCALE`.
const RANK_BASELINE: f64 = 1000.0;
const RANK_SCALE: f64 = 40.0;

/// Everything belonging to the current screen's overlay.
#[derive(Component)]
struct ScreenUi;

#[derive(Component)]
struct StatusText;

#[derive(Component)]
struct HudText;

/// The overlay shown to a player who is out of lives while the match plays on.
#[derive(Component)]
struct WaitingBanner;

/// The animated ellipsis on the queue screen, so a quiet search never looks
/// like a freeze.
#[derive(Component)]
struct QueueDots;

#[derive(Resource, Default)]
pub struct NickDraft(pub String);

#[derive(Resource)]
pub struct Ticket {
    pub id: String,
    timer: Timer,
}

#[derive(Resource, Default)]
pub struct Rating {
    pub found: bool,
    pub ordinal: f64,
    pub matches: i64,
}

/// The last thing that went wrong, shown to the player instead of being buried
/// in a log they will never read.
#[derive(Resource, Default)]
pub struct LastError(pub Option<String>);

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NickDraft>()
            .init_resource::<Rating>()
            .init_resource::<LastError>()
            .add_systems(Startup, boot)
            .add_systems(Update, handle_replies)
            .add_systems(OnEnter(Screen::Nick), show_nick)
            .add_systems(Update, type_nick.run_if(in_state(Screen::Nick)))
            .add_systems(OnEnter(Screen::Menu), show_menu)
            .add_systems(Update, menu_input.run_if(in_state(Screen::Menu)))
            .add_systems(OnEnter(Screen::Queue), show_queue)
            .add_systems(
                Update,
                (queue_tick, animate_queue).run_if(in_state(Screen::Queue)),
            )
            .add_systems(OnEnter(Screen::Playing), show_hud)
            .add_systems(
                Update,
                (update_hud, update_waiting).run_if(in_state(Screen::Playing)),
            )
            .add_systems(OnEnter(Screen::Result), show_result)
            .add_systems(Update, result_input.run_if(in_state(Screen::Result)))
            .add_systems(
                OnExit(Screen::Nick),
                clear_screen_ui,
            )
            .add_systems(OnExit(Screen::Menu), clear_screen_ui)
            .add_systems(OnExit(Screen::Queue), clear_screen_ui)
            .add_systems(OnExit(Screen::Playing), clear_screen_ui)
            .add_systems(OnExit(Screen::Result), clear_screen_ui);
    }
}

fn boot(
    mut commands: Commands,
    mut next: ResMut<NextState<Screen>>,
    mut session: ResMut<Session>,
    backend: Res<Backend>,
) {
    let saved = identity::load();
    if saved.is_usable() {
        session.token = saved.token.clone();
        session.player_id = saved.player_id.clone();
        session.nick = saved.nick.clone();
        info!("welcome back, {}", saved.nick);
        backend.send(Request::Rating, &session);
        next.set(Screen::Menu);
    } else {
        next.set(Screen::Nick);
    }
    commands.insert_resource(NickDraft::default());
}

fn clear_screen_ui(mut commands: Commands, ui: Query<Entity, With<ScreenUi>>) {
    for entity in ui.iter() {
        commands.entity(entity).despawn();
    }
}

/// A full screen centred column. Every menu screen is built from this.
fn panel() -> impl Bundle {
    (
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            row_gap: Val::Px(14.0),
            ..default()
        },
        ScreenUi,
    )
}

fn line(text: impl Into<String>, size: f32, colour: Color) -> impl Bundle {
    (
        Text::new(text.into()),
        TextFont {
            font_size: FontSize::Px(size),
            ..default()
        },
        TextColor(colour),
    )
}

// --- nick --------------------------------------------------------------------

fn show_nick(mut commands: Commands) {
    commands.spawn(panel()).with_children(|p| {
        p.spawn(line("GHOST CHASE 1v1", 52.0, ACCENT));
        p.spawn(line("what's your name?", 22.0, DIM));
        p.spawn((line("_", 34.0, FG), StatusText));
        p.spawn(line("type your name and press enter", 16.0, DIM));
    });
}

fn type_nick(
    mut keys: MessageReader<KeyboardInput>,
    mut draft: ResMut<NickDraft>,
    mut status: Query<&mut Text, With<StatusText>>,
    backend: Res<Backend>,
    session: Res<Session>,
) {
    let mut submit = false;

    for key in keys.read() {
        if key.state != ButtonState::Pressed {
            continue;
        }
        match &key.logical_key {
            Key::Character(c) => {
                for ch in c.chars() {
                    if !ch.is_control() && draft.0.chars().count() < NICK_MAX {
                        draft.0.push(ch);
                    }
                }
            }
            Key::Space if draft.0.chars().count() < NICK_MAX => draft.0.push(' '),
            Key::Backspace => {
                draft.0.pop();
            }
            Key::Enter => submit = true,
            _ => {}
        }
    }

    if let Ok(mut text) = status.single_mut() {
        **text = if draft.0.is_empty() {
            "_".to_string()
        } else {
            draft.0.clone()
        };
    }

    if submit && !draft.0.trim().is_empty() {
        backend.send(
            Request::Guest {
                nick: draft.0.trim().to_string(),
                player_id: None,
            },
            &session,
        );
    }
}

// --- menu --------------------------------------------------------------------

fn show_menu(mut commands: Commands, session: Res<Session>, rating: Res<Rating>, error: Res<LastError>) {
    let rank = if rating.found && rating.matches > 0 {
        let points = (RANK_BASELINE + rating.ordinal * RANK_SCALE).round() as i64;
        let unit = if rating.matches == 1 { "match" } else { "matches" };
        format!("Skill {}  ·  {} {}", points, rating.matches, unit)
    } else {
        "Unranked · play 1 to rank".to_string()
    };

    commands.spawn(panel()).with_children(|p| {
        p.spawn(line("GHOST CHASE 1v1", 52.0, ACCENT));
        p.spawn(line(format!("hi {}", session.nick), 24.0, FG));
        p.spawn(line(rank, 18.0, DIM));
        p.spawn(line("ENTER to find an opponent", 26.0, FG));
        if let Some(message) = error.0.as_ref() {
            p.spawn(line(message.clone(), 16.0, BAD));
        }
    });
}

fn menu_input(
    keys: Res<ButtonInput<KeyCode>>,
    backend: Res<Backend>,
    session: Res<Session>,
    mut next: ResMut<NextState<Screen>>,
    mut error: ResMut<LastError>,
) {
    if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) {
        error.0 = None;
        backend.send(Request::Enqueue, &session);
        next.set(Screen::Queue);
    }
}

// --- queue -------------------------------------------------------------------

fn show_queue(mut commands: Commands) {
    commands.spawn(panel()).with_children(|p| {
        p.spawn(line("finding an opponent", 40.0, ACCENT));
        p.spawn((line(".", 34.0, ACCENT), QueueDots));
        p.spawn((line("", 18.0, DIM), StatusText));
        p.spawn(line("ESC to cancel", 16.0, DIM));
    });
}

/// Cycles the queue ellipsis through one, two and three dots so the screen is
/// visibly alive between the once-a-second polls.
fn animate_queue(
    time: Res<Time>,
    mut elapsed: Local<f32>,
    mut step: Local<usize>,
    mut dots: Query<&mut Text, With<QueueDots>>,
) {
    *elapsed += time.delta_secs();
    if *elapsed < 0.3 {
        return;
    }
    *elapsed = 0.0;
    *step = (*step + 1) % 3;
    if let Ok(mut text) = dots.single_mut() {
        **text = ".".repeat(*step + 1);
    }
}

fn queue_tick(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    backend: Res<Backend>,
    session: Res<Session>,
    ticket: Option<ResMut<Ticket>>,
    mut commands: Commands,
    mut next: ResMut<NextState<Screen>>,
) {
    // ESC leaves the queue at any point, even after an assignment has consumed
    // the ticket and we are waiting on the server's welcome. Handling it before
    // the ticket guard is what makes it work in that window: otherwise a connect
    // that never completes leaves the player stuck with no way back.
    if keys.just_pressed(KeyCode::Escape) {
        if let Some(ticket) = ticket.as_ref() {
            backend.send(
                Request::Cancel {
                    ticket_id: ticket.id.clone(),
                },
                &session,
            );
        }
        disconnect(&mut commands);
        next.set(Screen::Menu);
        return;
    }

    let Some(mut ticket) = ticket else {
        return;
    };

    ticket.timer.tick(time.delta());
    if ticket.timer.just_finished() {
        backend.send(
            Request::Poll {
                ticket_id: ticket.id.clone(),
            },
            &session,
        );
    }
}

// --- hud ---------------------------------------------------------------------

fn show_hud(mut commands: Commands) {
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(10.0)),
                row_gap: Val::Px(4.0),
                ..default()
            },
            ScreenUi,
        ))
        .with_children(|p| {
            p.spawn((line("", 24.0, FG), HudText));
        });
}

fn update_hud(game: Option<Res<Match>>, mut hud: Query<&mut Text, With<HudText>>) {
    let (Some(game), Ok(mut text)) = (game, hud.single_mut()) else {
        return;
    };

    let me = game.slot as usize;
    let rival = game.rival_slot();
    let seconds = game.elapsed_ms as f32 / 1000.0;
    let difficulty = Difficulty::at(seconds);

    **text = format!(
        "{}  {}  {}      {:02}:{:02}  level {}  {} ghosts      {}  {}  {}",
        game.nicks[me],
        game.scores[me],
        "*".repeat(game.lives[me] as usize),
        (seconds as u32) / 60,
        (seconds as u32) % 60,
        difficulty.step + 1,
        difficulty.ghost_count,
        game.nicks[rival],
        game.scores[rival],
        "*".repeat(game.lives[rival] as usize),
    );
}

/// A player who is out of lives has nothing left to do but watch: the match
/// only ends once *both* players are out. Cover their board with a banner so it
/// is clear the game has not frozen, and clear it again if they somehow return
/// (they cannot today, but the check keeps this honest).
fn update_waiting(
    mut commands: Commands,
    game: Option<Res<Match>>,
    banner: Query<Entity, With<WaitingBanner>>,
) {
    let out = game
        .as_ref()
        .map(|g| g.states[g.slot as usize] == RunnerState::Out)
        .unwrap_or(false);

    match (out, banner.single()) {
        (true, Err(_)) => {
            commands
                .spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::Center,
                        row_gap: Val::Px(10.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.02, 0.02, 0.05, 0.72)),
                    ScreenUi,
                    WaitingBanner,
                ))
                .with_children(|p| {
                    p.spawn(line("YOU'RE OUT", 44.0, BAD));
                    p.spawn(line("waiting for the other player", 22.0, DIM));
                });
        }
        (false, Ok(entity)) => commands.entity(entity).despawn(),
        _ => {}
    }
}

// --- result ------------------------------------------------------------------

fn show_result(mut commands: Commands, game: Option<Res<Match>>, backend: Res<Backend>, session: Res<Session>) {
    let (headline, detail) = match game.as_ref().and_then(|g| g.result.map(|r| (g, r))) {
        Some((game, result)) => {
            let me = game.slot as usize;
            let rival = game.rival_slot();
            let headline = match result.winner {
                Some(w) if w as usize == me => "YOU WON".to_string(),
                Some(_) => "YOU LOST".to_string(),
                None => "DRAW".to_string(),
            };
            let detail = format!(
                "{} {}   vs   {} {}",
                game.nicks[me], result.scores[me], game.nicks[rival], result.scores[rival]
            );
            (headline, detail)
        }
        None => ("MATCH OVER".to_string(), String::new()),
    };

    // The rating only changes once the server's report has landed, so ask again
    // now rather than showing the stale number from before the match.
    backend.send(Request::Rating, &session);

    commands.spawn(panel()).with_children(|p| {
        p.spawn(line(headline, 56.0, ACCENT));
        p.spawn(line(detail, 24.0, FG));
        p.spawn(line("ENTER to return to the menu", 18.0, DIM));
    });
}

fn result_input(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut next: ResMut<NextState<Screen>>,
) {
    if keys.just_pressed(KeyCode::Enter)
        || keys.just_pressed(KeyCode::Space)
        || keys.just_pressed(KeyCode::Escape)
    {
        disconnect(&mut commands);
        next.set(Screen::Menu);
    }
}

// --- replies -----------------------------------------------------------------

// A Bevy system's arguments are its dependency injection: every one is a
// distinct resource this handler needs. Splitting it would not make it clearer.
#[allow(clippy::too_many_arguments)]
fn handle_replies(
    mut commands: Commands,
    mut replies: MessageReader<Reply>,
    mut session: ResMut<Session>,
    mut next: ResMut<NextState<Screen>>,
    mut rating: ResMut<Rating>,
    mut error: ResMut<LastError>,
    mut status: Query<&mut Text, With<StatusText>>,
    state: Res<State<Screen>>,
    assignment: Option<Res<Assignment>>,
) {
    // The queue poll keeps several requests in flight, so the same assignment
    // comes back many times. Connect exactly once: a second `connect` replaces
    // the transport and restarts the netcode handshake, so it never finishes
    // and the client is stuck in the queue forever. Once an assignment exists we
    // are already connecting, so ignore any further ones.
    let mut connecting = assignment.is_some();
    for reply in replies.read() {
        match reply {
            Reply::Identity {
                player_id,
                nick,
                token,
            } => {
                session.player_id = player_id.clone();
                session.nick = nick.clone();
                session.token = token.clone();
                identity::save(&identity::Identity {
                    player_id: player_id.clone(),
                    nick: nick.clone(),
                    token: token.clone(),
                });
                next.set(Screen::Menu);
            }

            Reply::Queued { ticket_id } => {
                commands.insert_resource(Ticket {
                    id: ticket_id.clone(),
                    timer: Timer::from_seconds(POLL_EVERY, TimerMode::Repeating),
                });
            }

            Reply::Searching => {
                if let Ok(mut text) = status.single_mut() {
                    **text = "still searching".into();
                }
            }

            Reply::Assigned {
                connection,
                session_token,
            } => {
                if connecting {
                    continue;
                }
                let assignment = Assignment {
                    connection: connection.clone(),
                    session_token: session_token.clone(),
                    player_id: session.player_id.clone(),
                };
                match connect(&mut commands, &assignment) {
                    Ok(()) => {
                        connecting = true;
                        commands.insert_resource(assignment);
                        commands.remove_resource::<Ticket>();
                        if let Ok(mut text) = status.single_mut() {
                            **text = "entering the match".into();
                        }
                    }
                    Err(message) => {
                        error!("could not connect: {message}");
                        error.0 = Some(message.clone());
                        next.set(Screen::Menu);
                    }
                }
            }

            Reply::QueueTimedOut => {
                error.0 = Some("nobody showed up, try again".into());
                commands.remove_resource::<Ticket>();
                next.set(Screen::Menu);
            }

            Reply::Cancelled => {
                commands.remove_resource::<Ticket>();
            }

            Reply::Rating {
                found,
                ordinal,
                matches,
            } => {
                rating.found = *found;
                rating.ordinal = *ordinal;
                rating.matches = *matches;
                // Refresh the menu so the new number is actually on screen.
                if *state.get() == Screen::Menu {
                    next.set(Screen::Menu);
                }
            }

            Reply::Failed { what, message } => {
                error!("{what} failed: {message}");
                error.0 = Some(format!("{what}: {message}"));
                if *state.get() != Screen::Nick {
                    next.set(Screen::Menu);
                }
            }
        }
    }
}
