//! Talking to our own backend.
//!
//! Bevy's schedule must never block, and reqwest needs a tokio runtime, so this
//! owns a small one and bridges it to ECS with a channel. Systems post a
//! `Request` and read `Reply` messages later; nothing in the game loop ever
//! awaits.

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

/// What the UI asks the backend to do.
#[derive(Debug, Clone)]
pub enum Request {
    /// Create or refresh a guest identity.
    Guest {
        nick: String,
        player_id: Option<String>,
    },
    Enqueue,
    Poll {
        ticket_id: String,
    },
    Cancel {
        ticket_id: String,
    },
    Rating,
}

/// What came back.
#[derive(Debug, Clone, Message)]
pub enum Reply {
    Identity {
        player_id: String,
        nick: String,
        token: String,
    },
    Queued {
        ticket_id: String,
    },
    Searching,
    Assigned {
        connection: String,
        session_token: String,
    },
    QueueTimedOut,
    Cancelled,
    Rating {
        found: bool,
        ordinal: f64,
        matches: i64,
    },
    Failed {
        what: &'static str,
        message: String,
    },
}

#[derive(Serialize)]
struct GuestBody<'a> {
    nick: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    player_id: &'a Option<String>,
}

#[derive(Deserialize)]
struct GuestReply {
    player_id: String,
    nick: String,
    token: String,
}

#[derive(Deserialize)]
struct QueueReply {
    ticket_id: String,
}

#[derive(Deserialize)]
struct TicketReply {
    status: String,
    #[serde(default)]
    connection: Option<String>,
    #[serde(default)]
    session_token: Option<String>,
}

#[derive(Deserialize)]
struct RatingReply {
    found: bool,
    #[serde(default)]
    ordinal: f64,
    #[serde(default)]
    matches: i64,
}

/// The player's token, once we have one.
#[derive(Resource, Default, Debug, Clone)]
pub struct Session {
    pub token: String,
    pub player_id: String,
    pub nick: String,
}

#[derive(Resource)]
pub struct Backend {
    runtime: Runtime,
    base_url: String,
    tx: Sender<Reply>,
    rx: Receiver<Reply>,
}

impl Backend {
    /// Queues a request. Returns immediately; the answer arrives as a `Reply`
    /// message on a later frame.
    pub fn send(&self, request: Request, session: &Session) {
        let base = self.base_url.clone();
        let tx = self.tx.clone();
        let token = session.token.clone();

        self.runtime.spawn(async move {
            let client = reqwest::Client::new();
            let reply = perform(&client, &base, &token, request).await;
            let _ = tx.send(reply);
        });
    }
}

async fn perform(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    request: Request,
) -> Reply {
    match request {
        Request::Guest { nick, player_id } => {
            let body = GuestBody {
                nick: &nick,
                player_id: &player_id,
            };
            match post_json::<_, GuestReply>(client, &format!("{base}/auth/guest"), "", &body).await
            {
                Ok(r) => Reply::Identity {
                    player_id: r.player_id,
                    nick: r.nick,
                    token: r.token,
                },
                Err(message) => Reply::Failed {
                    what: "identity",
                    message,
                },
            }
        }

        Request::Enqueue => {
            match post_json::<_, QueueReply>(
                client,
                &format!("{base}/queue"),
                token,
                &serde_json::json!({}),
            )
            .await
            {
                Ok(r) => Reply::Queued {
                    ticket_id: r.ticket_id,
                },
                Err(message) => Reply::Failed {
                    what: "queue",
                    message,
                },
            }
        }

        Request::Poll { ticket_id } => {
            let url = format!("{base}/queue/{ticket_id}");
            match get_json::<TicketReply>(client, &url, token).await {
                Ok(r) => match r.status.as_str() {
                    "assigned" => match (r.connection, r.session_token) {
                        (Some(connection), Some(session_token)) => Reply::Assigned {
                            connection,
                            session_token,
                        },
                        // Assigned without the pieces needed to connect would
                        // strand the player on a spinner, so treat it as a
                        // failure rather than waiting forever.
                        _ => Reply::Failed {
                            what: "queue",
                            message: "the server was assigned without a connection".into(),
                        },
                    },
                    "timeout" => Reply::QueueTimedOut,
                    _ => Reply::Searching,
                },
                Err(message) => Reply::Failed {
                    what: "queue",
                    message,
                },
            }
        }

        Request::Cancel { ticket_id } => {
            let url = format!("{base}/queue/{ticket_id}");
            let _ = client.delete(&url).bearer_auth(token).send().await;
            Reply::Cancelled
        }

        Request::Rating => {
            match get_json::<RatingReply>(client, &format!("{base}/me/rating"), token).await {
                Ok(r) => Reply::Rating {
                    found: r.found,
                    ordinal: r.ordinal,
                    matches: r.matches,
                },
                Err(message) => Reply::Failed {
                    what: "rating",
                    message,
                },
            }
        }
    }
}

async fn post_json<B: Serialize, T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    body: &B,
) -> Result<T, String> {
    let mut req = client.post(url).json(body);
    if !token.is_empty() {
        req = req.bearer_auth(token);
    }
    finish(req).await
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<T, String> {
    finish(client.get(url).bearer_auth(token)).await
}

async fn finish<T: serde::de::DeserializeOwned>(
    req: reqwest::RequestBuilder,
) -> Result<T, String> {
    let res = req.send().await.map_err(|e| e.to_string())?;
    let status = res.status();
    let body = res.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(format!("{status}: {body}"));
    }
    serde_json::from_str(&body).map_err(|e| format!("could not read the reply: {e}"))
}

pub struct BackendPlugin {
    pub base_url: String,
}

impl Plugin for BackendPlugin {
    fn build(&self, app: &mut App) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .thread_name("ghostchase-backend")
            .build()
            .expect("building the backend runtime");

        let (tx, rx) = crossbeam_channel::unbounded();

        app.insert_resource(Backend {
            runtime,
            base_url: self.base_url.trim_end_matches('/').to_string(),
            tx,
            rx,
        })
        .init_resource::<Session>()
        .add_message::<Reply>()
        .add_systems(PreUpdate, pump_replies);
    }
}

/// Moves finished requests off the channel and into ECS. `try_recv` only: the
/// schedule must never wait on the network.
fn pump_replies(backend: Res<Backend>, mut out: MessageWriter<Reply>) {
    while let Ok(reply) = backend.rx.try_recv() {
        out.write(reply);
    }
}
