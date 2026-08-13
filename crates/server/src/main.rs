//! Ghost Chase 1v1 dedicated server.
//!
//! Headless Bevy. It connects to GameFlow, binds the port it was given, runs
//! the authoritative simulation at a fixed 30Hz, reports the result through its
//! own backend and shuts itself down.
//!
//! It never holds the GameFlow API key. It never trusts a client.

mod gameflow_plugin;
mod net;
mod report;
mod roster;
mod session;

use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::log::LogPlugin;
use bevy::prelude::*;

use gameflow_plugin::{
    GameFlowConnected, GameFlowConnectionFailed, GameFlowHealthDegraded, GameFlowPlugin,
    GameFlowReady, GameFlowShutdown,
};

/// Environment the server reads. No API key here by design: results go through
/// the backend, which is the only holder.
#[derive(Resource, Debug, Clone)]
pub struct ServerConfigRes {
    pub backend_url: String,
    pub api_token: String,
    pub match_seed: u64,
}

impl ServerConfigRes {
    fn from_env() -> ServerConfigRes {
        let backend_url = std::env::var("GAME_BACKEND_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".into())
            .trim_end_matches('/')
            .to_string();

        let api_token = std::env::var("GAME_BACKEND_API_TOKEN").unwrap_or_default();
        if api_token.is_empty() {
            // Without it every handshake fails, so say so once and loudly
            // instead of leaving a silent server nobody can join.
            eprintln!(
                "GAME_BACKEND_API_TOKEN is not set: every client handshake will be rejected"
            );
        }

        // Only used to vary pellet placement between matches. It is sent to the
        // clients in the welcome, so it is not a secret.
        let match_seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x5EED);

        ServerConfigRes {
            backend_url,
            api_token,
            match_seed,
        }
    }
}

fn main() {
    let config = ServerConfigRes::from_env();

    App::new()
        // The runner polls faster than the simulation so FixedUpdate lands on
        // clean 30Hz steps instead of drifting with the loop.
        .add_plugins(MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(
            Duration::from_secs_f64(1.0 / 60.0),
        )))
        .add_plugins(LogPlugin::default())
        // manual_ready: the server is not ready until its socket is bound.
        .add_plugins(GameFlowPlugin::default().manual_ready())
        .insert_resource(config)
        .add_plugins((net::NetServerPlugin, session::SessionPlugin, report::ReportPlugin))
        .add_observer(on_connected)
        .add_observer(on_ready)
        .add_observer(on_connection_failed)
        .add_observer(on_health_degraded)
        .add_observer(on_shutdown)
        .run();
}

fn on_connected(_: On<GameFlowConnected>, client: Option<Res<gameflow_plugin::GameFlowClient>>) {
    if let Some(client) = client {
        info!(
            "GameFlow connected in {:?} mode, port {:?}, region {:?}",
            client.mode(),
            client.default_port(),
            client.region()
        );
    }
}

fn on_ready(_: On<GameFlowReady>) {
    info!("marked ready, health heartbeat running");
}

fn on_connection_failed(event: On<GameFlowConnectionFailed>, mut exit: MessageWriter<AppExit>) {
    // On the platform this means the sidecar is unreachable. A server that
    // cannot report health will be killed anyway, and failing fast makes that
    // visible instead of leaving a zombie accepting players.
    error!("could not connect to GameFlow: {}", event.message);
    exit.write(AppExit::error());
}

fn on_health_degraded(_: On<GameFlowHealthDegraded>) {
    warn!("health pings have been failing, the platform may recycle this pod");
}

fn on_shutdown(_: On<GameFlowShutdown>, mut exit: MessageWriter<AppExit>) {
    info!("shutdown complete, exiting");
    exit.write(AppExit::Success);
}
