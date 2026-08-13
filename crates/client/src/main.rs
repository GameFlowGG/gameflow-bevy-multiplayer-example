//! Ghost Chase 1v1 client.
//!
//! A desktop Bevy app. It renders the maze, predicts its own runner and sends
//! one thing over the wire: the direction the player wants to go. It never
//! reports a score, and it never holds a GameFlow key. Everything it needs from
//! the platform comes through our own backend.

mod backend;
mod identity;
mod net;
mod predict;
mod render;
mod ui;

use bevy::prelude::*;
use bevy::window::WindowResolution;

use render::TILE;

/// The one place the client is told where its backend lives.
const DEFAULT_BACKEND: &str = "http://127.0.0.1:8080";

#[derive(States, Default, Debug, Clone, PartialEq, Eq, Hash)]
pub enum Screen {
    /// Nothing on screen yet; the identity file is being read.
    #[default]
    Boot,
    Nick,
    Menu,
    Queue,
    Playing,
    Result,
}

fn main() {
    let backend_url =
        std::env::var("GHOSTCHASE_BACKEND_URL").unwrap_or_else(|_| DEFAULT_BACKEND.to_string());

    let width = ghostchase_shared::MAZE_W as f32 * TILE;
    let height = ghostchase_shared::MAZE_H as f32 * TILE + 60.0;

    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Ghost Chase 1v1".into(),
                        resolution: WindowResolution::new(width as u32, height as u32),
                        resizable: false,
                        ..default()
                    }),
                    ..default()
                })
                .set(ImagePlugin::default_nearest()),
        )
        // The simulation rate is fixed and shared with the server. Prediction
        // has to step at exactly the same rate or it would drift by design.
        .insert_resource(Time::<Fixed>::from_hz(ghostchase_shared::TICK_HZ as f64))
        .insert_resource(ClearColor(Color::srgb(0.04, 0.04, 0.07)))
        .init_state::<Screen>()
        .add_plugins(backend::BackendPlugin { base_url: backend_url })
        .add_plugins((net::NetClientPlugin, render::RenderPlugin, ui::UiPlugin))
        .add_systems(Startup, spawn_camera)
        .add_systems(Update, net::advance_interpolation)
        .run();
}

fn spawn_camera(mut commands: Commands) {
    commands.spawn(Camera2d);
}
