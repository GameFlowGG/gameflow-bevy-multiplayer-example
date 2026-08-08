//! Reusable GameFlow <-> Bevy bridge — a copy-paste template.
//!
//! The GameFlow SDK (`gameflow`) is engine-agnostic. This module is the thin
//! glue that wraps it as a Bevy plugin so the ECS side never touches `async`.
//! It is intentionally NOT a published crate: copy this file into your own Bevy
//! project (its only deps are `gameflow-gameserver-sdk`, `bevy`, `tokio` and
//! `crossbeam-channel`) and it compiles against *your* Bevy version — there is
//! no plugin crate to keep in lockstep with Bevy releases.
//!
//! - [`GameFlowPlugin`] connects in the background on its own tokio runtime
//!   (reqwest cannot run on Bevy's task pools) and bridges the SDK into ECS.
//! - a [`GameFlowClient`] resource appears once connected, with non-blocking
//!   command methods and synchronous cache reads.
//! - updates arrive as **messages** ([`PlayerConnected`], [`ServerInfoChanged`],
//!   ...); lifecycle moments arrive as observable **events**
//!   ([`GameFlowConnected`], [`GameFlowReady`], [`GameFlowShutdown`], ...).

#![allow(dead_code)] // a reusable template: not every helper is used by the demo

use std::time::Duration;

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender};
use tokio::runtime::{Handle, Runtime};

use gameflow::{ConnectOptions, GameFlow};

pub use gameflow::{ErrorCode, Mode, ModeOption, ServerInfo};

/// Configuration for [`GameFlowPlugin`].
#[derive(Debug, Clone)]
pub struct GameFlowConfig {
    /// Transport selection. Defaults to auto-detection.
    pub mode: ModeOption,
    /// Interval between automatic health pings (>= 500ms).
    pub health_interval: Duration,
    /// Total budget for the initial connect, including retries.
    pub connect_timeout: Duration,
    /// Per-request timeout.
    pub request_timeout: Duration,
    /// Call `ready()` automatically as soon as the SDK connects.
    ///
    /// Convenient for servers that can accept players the moment the app boots.
    /// Set to `false` and call [`GameFlowClient::ready`] yourself once your
    /// server is actually listening.
    pub auto_ready: bool,
}

impl Default for GameFlowConfig {
    fn default() -> Self {
        GameFlowConfig {
            mode: ModeOption::Auto,
            health_interval: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(3),
            auto_ready: true,
        }
    }
}

/// The GameFlow Bevy plugin. Add it once to your `App`.
#[derive(Debug, Default)]
pub struct GameFlowPlugin {
    /// Connection configuration.
    pub config: GameFlowConfig,
}

impl GameFlowPlugin {
    /// A plugin with the given configuration.
    pub fn new(config: GameFlowConfig) -> Self {
        GameFlowPlugin { config }
    }

    /// Do not call `ready()` automatically; the app drives it via
    /// [`GameFlowClient::ready`].
    pub fn manual_ready(mut self) -> Self {
        self.config.auto_ready = false;
        self
    }
}

impl Plugin for GameFlowPlugin {
    fn build(&self, app: &mut App) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("gameflow-sdk")
            .build()
            .expect("failed to build the GameFlow tokio runtime");

        let (sender, inbox) = crossbeam_channel::unbounded::<SdkEvent>();

        // Kick off the connection on the runtime's threads (reqwest needs them).
        runtime.spawn(connect_task(self.config.clone(), sender.clone()));

        app.insert_resource(GameFlowSettings(self.config.clone()))
            .insert_resource(GameFlowBridge {
                runtime,
                inbox,
                sender,
            })
            .add_message::<PlayerConnected>()
            .add_message::<PlayerConnectFailed>()
            .add_message::<PlayerDisconnected>()
            .add_message::<ServerInfoChanged>()
            .add_message::<PayloadChanged>()
            .add_message::<GameFlowOperationFailed>()
            .add_systems(Update, pump_sdk_inbox);
    }
}

// --- Resources ---------------------------------------------------------------

#[derive(Resource)]
struct GameFlowSettings(GameFlowConfig);

/// Owns the tokio runtime and the SDK -> ECS channel. Kept alive for the whole
/// app so the health loop and watch stream keep running.
#[derive(Resource)]
struct GameFlowBridge {
    runtime: Runtime,
    inbox: Receiver<SdkEvent>,
    sender: Sender<SdkEvent>,
}

/// The live SDK handle, inserted once the SDK has connected.
///
/// Command methods are non-blocking: they enqueue async work on the runtime and
/// surface the result as a message or event. Read methods are synchronous and
/// served from the SDK's local cache.
#[derive(Resource, Clone)]
pub struct GameFlowClient {
    gf: GameFlow,
    handle: Handle,
    sender: Sender<SdkEvent>,
}

impl GameFlowClient {
    /// Marks the server ready and starts the automatic health heartbeat.
    pub fn ready(&self) {
        let (gf, tx) = (self.gf.clone(), self.sender.clone());
        self.handle.spawn(async move {
            match gf.ready().await {
                Ok(()) => {
                    let _ = tx.send(SdkEvent::Ready);
                }
                Err(error) => {
                    let _ = tx.send(SdkEvent::OperationFailed {
                        operation: "ready",
                        message: error.to_string(),
                    });
                }
            }
        });
    }

    /// Registers a connected player session.
    pub fn connect_player(&self, session_id: impl Into<String>) {
        let (gf, tx, id) = (self.gf.clone(), self.sender.clone(), session_id.into());
        self.handle.spawn(async move {
            match gf.players().connect(&id).await {
                Ok(()) => {
                    let _ = tx.send(SdkEvent::PlayerConnected(id));
                }
                Err(error) => {
                    let _ = tx.send(SdkEvent::PlayerConnectFailed {
                        session_id: id,
                        code: error.code(),
                        message: error.to_string(),
                    });
                }
            }
        });
    }

    /// Unregisters a player session (idempotent).
    pub fn disconnect_player(&self, session_id: impl Into<String>) {
        let (gf, tx, id) = (self.gf.clone(), self.sender.clone(), session_id.into());
        self.handle.spawn(async move {
            match gf.players().disconnect(&id).await {
                Ok(found) => {
                    let _ = tx.send(SdkEvent::PlayerDisconnected {
                        session_id: id,
                        found,
                    });
                }
                Err(error) => {
                    let _ = tx.send(SdkEvent::PlayerConnectFailed {
                        session_id: id,
                        code: error.code(),
                        message: error.to_string(),
                    });
                }
            }
        });
    }

    /// Shuts the server down cleanly.
    pub fn shutdown(&self) {
        let (gf, tx) = (self.gf.clone(), self.sender.clone());
        self.handle.spawn(async move {
            let _ = gf.shutdown().await;
            let _ = tx.send(SdkEvent::ShutdownComplete);
        });
    }

    /// Number of connected players (synchronous, from the local cache).
    pub fn player_count(&self) -> usize {
        self.gf.players().count()
    }

    /// Session ids of connected players (synchronous, from the local cache).
    pub fn players(&self) -> Vec<String> {
        self.gf.players().list()
    }

    /// Maximum players configured for this game (`-1` means no configured limit).
    pub fn capacity(&self) -> i64 {
        self.gf.players().capacity()
    }

    /// Whether player tracking is enabled.
    pub fn tracking_enabled(&self) -> bool {
        self.gf.players().tracking_enabled()
    }

    /// Transport in use.
    pub fn mode(&self) -> Mode {
        self.gf.mode()
    }

    /// Region this server runs in, when provided by the platform.
    pub fn region(&self) -> Option<String> {
        self.gf.region()
    }

    /// Build id of the running image, when provided by the platform.
    pub fn build_id(&self) -> Option<String> {
        self.gf.build_id()
    }

    /// The game's primary assigned port (`GAMEFLOW_DEFAULT_PORT`).
    pub fn default_port(&self) -> Option<u16> {
        self.gf.ports().default()
    }

    /// The underlying SDK handle, for advanced async use on your own runtime.
    pub fn handle(&self) -> &GameFlow {
        &self.gf
    }
}

// --- Messages (buffered; poll with MessageReader) ----------------------------

/// A player session was registered with the platform.
#[derive(Message, Debug, Clone)]
pub struct PlayerConnected {
    /// The registered session id.
    pub session_id: String,
}

/// Registering a player session failed.
#[derive(Message, Debug, Clone)]
pub struct PlayerConnectFailed {
    /// The session id that failed to register.
    pub session_id: String,
    /// The stable error code.
    pub code: ErrorCode,
    /// A human-readable message.
    pub message: String,
}

/// A player session was unregistered.
#[derive(Message, Debug, Clone)]
pub struct PlayerDisconnected {
    /// The unregistered session id.
    pub session_id: String,
    /// `false` when the player was not in the list (idempotent disconnect).
    pub found: bool,
}

/// The server details changed (from the watch stream).
#[derive(Message, Debug, Clone)]
pub struct ServerInfoChanged {
    /// The new server details.
    pub info: ServerInfo,
}

/// The launch payload changed (e.g. on match assignment).
#[derive(Message, Debug, Clone)]
pub struct PayloadChanged {
    /// The new payload, or `None` when cleared.
    pub payload: Option<String>,
}

/// A lifecycle operation (`ready`/`shutdown`) failed.
#[derive(Message, Debug, Clone)]
pub struct GameFlowOperationFailed {
    /// The operation that failed.
    pub operation: String,
    /// A human-readable message.
    pub message: String,
}

// --- Events (observable; react with add_observer) ----------------------------

/// The SDK connected to GameFlow (or local mode). The [`GameFlowClient`]
/// resource is available from now on.
#[derive(Event, Debug)]
pub struct GameFlowConnected;

/// The SDK failed to connect.
#[derive(Event, Debug)]
pub struct GameFlowConnectionFailed {
    /// A human-readable message.
    pub message: String,
}

/// `ready()` succeeded; the automatic health heartbeat is running.
#[derive(Event, Debug)]
pub struct GameFlowReady;

/// Health pings have been failing for a sustained period.
#[derive(Event, Debug)]
pub struct GameFlowHealthDegraded;

/// The server has been shut down.
#[derive(Event, Debug)]
pub struct GameFlowShutdown;

// --- Bridge internals --------------------------------------------------------

/// Messages flowing from the SDK (tokio) into ECS. All payloads are plain owned
/// data so they are `Send`.
enum SdkEvent {
    Connected(GameFlow),
    ConnectFailed(String),
    Ready,
    OperationFailed {
        operation: &'static str,
        message: String,
    },
    PlayerConnected(String),
    PlayerConnectFailed {
        session_id: String,
        code: ErrorCode,
        message: String,
    },
    PlayerDisconnected {
        session_id: String,
        found: bool,
    },
    ServerInfo(ServerInfo),
    PayloadChanged(Option<String>),
    HealthDegraded,
    ShutdownComplete,
}

async fn connect_task(config: GameFlowConfig, tx: Sender<SdkEvent>) {
    let options = ConnectOptions::new()
        .mode(config.mode)
        .health_interval(config.health_interval)
        .connect_timeout(config.connect_timeout)
        .request_timeout(config.request_timeout)
        .on_health_degraded({
            let tx = tx.clone();
            std::sync::Arc::new(move || {
                let _ = tx.send(SdkEvent::HealthDegraded);
            })
        });

    match GameFlow::connect_with(options).await {
        Ok(gf) => {
            // Forward watch + payload updates into the channel. Detach the
            // subscriptions so they live for the lifetime of the app.
            if let Ok(sub) = gf.watch({
                let tx = tx.clone();
                move |info| {
                    let _ = tx.send(SdkEvent::ServerInfo(info));
                }
            }) {
                sub.detach();
            }
            if let Ok(sub) = gf.on_payload_change({
                let tx = tx.clone();
                move |payload| {
                    let _ = tx.send(SdkEvent::PayloadChanged(payload));
                }
            }) {
                sub.detach();
            }
            let _ = tx.send(SdkEvent::Connected(gf));
        }
        Err(error) => {
            let _ = tx.send(SdkEvent::ConnectFailed(error.to_string()));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn pump_sdk_inbox(
    mut commands: Commands,
    bridge: Res<GameFlowBridge>,
    settings: Res<GameFlowSettings>,
    mut player_connected: MessageWriter<PlayerConnected>,
    mut player_connect_failed: MessageWriter<PlayerConnectFailed>,
    mut player_disconnected: MessageWriter<PlayerDisconnected>,
    mut server_info: MessageWriter<ServerInfoChanged>,
    mut payload_changed: MessageWriter<PayloadChanged>,
    mut operation_failed: MessageWriter<GameFlowOperationFailed>,
) {
    // try_recv only: never block the schedule.
    while let Ok(event) = bridge.inbox.try_recv() {
        match event {
            SdkEvent::Connected(gf) => {
                let client = GameFlowClient {
                    gf,
                    handle: bridge.runtime.handle().clone(),
                    sender: bridge.sender.clone(),
                };
                if settings.0.auto_ready {
                    client.ready();
                }
                commands.insert_resource(client);
                commands.trigger(GameFlowConnected);
            }
            SdkEvent::ConnectFailed(message) => {
                commands.trigger(GameFlowConnectionFailed { message });
            }
            SdkEvent::Ready => commands.trigger(GameFlowReady),
            SdkEvent::OperationFailed { operation, message } => {
                operation_failed.write(GameFlowOperationFailed {
                    operation: operation.to_string(),
                    message,
                });
            }
            SdkEvent::PlayerConnected(session_id) => {
                player_connected.write(PlayerConnected { session_id });
            }
            SdkEvent::PlayerConnectFailed {
                session_id,
                code,
                message,
            } => {
                player_connect_failed.write(PlayerConnectFailed {
                    session_id,
                    code,
                    message,
                });
            }
            SdkEvent::PlayerDisconnected { session_id, found } => {
                player_disconnected.write(PlayerDisconnected { session_id, found });
            }
            SdkEvent::ServerInfo(info) => {
                server_info.write(ServerInfoChanged { info });
            }
            SdkEvent::PayloadChanged(payload) => {
                payload_changed.write(PayloadChanged { payload });
            }
            SdkEvent::HealthDegraded => commands.trigger(GameFlowHealthDegraded),
            SdkEvent::ShutdownComplete => commands.trigger(GameFlowShutdown),
        }
    }
}
