//! Reporting the result and shutting down.
//!
//! At game over the server posts the raw result to its own backend, which is
//! the only holder of the GameFlow API key. The server never talks to the
//! rating API itself.
//!
//! Shutting down matters as much as reporting. A pod that stays up after its
//! match burns the organisation's quota, so the shutdown path runs even when
//! the report fails.

use std::time::Duration;

use bevy::prelude::*;
use crossbeam_channel::{Receiver, Sender};
use serde::Serialize;
use tokio::runtime::Runtime;

use ghostchase_shared::sim::MatchPhase;

use crate::gameflow_plugin::GameFlowClient;
use crate::session::Session;
use crate::ServerConfigRes;

const ATTEMPTS: u32 = 3;
const BACKOFF: Duration = Duration::from_millis(500);

#[derive(Debug, Serialize)]
struct ResultPlayer {
    player_id: String,
    nick: String,
    score: u32,
    present: bool,
}

#[derive(Debug, Serialize)]
struct MatchResult {
    match_id: String,
    players: Vec<ResultPlayer>,
    winner_slot: Option<u8>,
}

/// What the reporting task tells the ECS when it is done.
struct ReportDone {
    ok: bool,
}

#[derive(Resource)]
struct Reporter {
    runtime: Runtime,
    tx: Sender<ReportDone>,
    rx: Receiver<ReportDone>,
    /// Set once shutdown has been asked for, so it is asked for only once.
    winding_down: bool,
}

pub struct ReportPlugin;

impl Plugin for ReportPlugin {
    fn build(&self, app: &mut App) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(1)
            .thread_name("ghostchase-report")
            .build()
            .expect("building the reporting runtime");

        let (tx, rx) = crossbeam_channel::unbounded();
        install_sigterm(tx.clone(), &runtime);

        app.insert_resource(Reporter {
            runtime,
            tx,
            rx,
            winding_down: false,
        })
        .add_systems(Update, (send_result_when_finished, finish_shutdown).chain());
    }
}

/// SIGTERM means the platform wants the pod back. Draining and calling
/// shutdown has a 45 second budget, so the same wind-down path is reused.
fn install_sigterm(tx: Sender<ReportDone>, runtime: &Runtime) {
    runtime.spawn(async move {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
                let _ = tx.send(ReportDone { ok: false });
            }
            Err(e) => {
                // Not fatal: without the handler the process still exits on
                // SIGTERM, just without a clean drain.
                eprintln!("could not install the SIGTERM handler: {e}");
            }
        }
    });
}

fn send_result_when_finished(
    session: Option<ResMut<Session>>,
    config: Res<ServerConfigRes>,
    reporter: Res<Reporter>,
) {
    let Some(mut session) = session else {
        return;
    };
    let MatchPhase::Finished { winner } = session.sim.phase else {
        return;
    };
    if session.reported {
        return;
    }
    session.reported = true;

    let players: Vec<ResultPlayer> = (0..2usize)
        .map(|slot| ResultPlayer {
            player_id: session.slots[slot].player_id.clone(),
            nick: session.slots[slot].nick.clone(),
            score: session.sim.runners[slot].score,
            present: session.slots[slot].ever_present,
        })
        .collect();

    let rateable = players.iter().all(|p| p.present);
    let body = MatchResult {
        match_id: if session.match_id.is_empty() {
            format!("local-{}", session.seed)
        } else {
            session.match_id.clone()
        },
        players,
        winner_slot: winner,
    };

    if !rateable {
        info!("not reporting: a slot was never occupied, the match says nothing about skill");
        let _ = reporter.tx.send(ReportDone { ok: false });
        return;
    }

    let url = format!("{}/internal/match-result", config.backend_url);
    let token = config.api_token.clone();
    let tx = reporter.tx.clone();

    info!(
        "reporting match {} to {url}",
        body.match_id
    );

    reporter.runtime.spawn(async move {
        let client = reqwest::Client::new();
        let mut ok = false;

        for attempt in 1..=ATTEMPTS {
            let res = client
                .post(&url)
                .header("X-Game-Backend-Token", &token)
                .json(&body)
                .send()
                .await;

            match res {
                Ok(r) if r.status().is_success() => {
                    ok = true;
                    break;
                }
                Ok(r) => eprintln!("report attempt {attempt} rejected: {}", r.status()),
                Err(e) => eprintln!("report attempt {attempt} failed: {e}"),
            }

            if attempt < ATTEMPTS {
                tokio::time::sleep(BACKOFF * attempt).await;
            }
        }

        if !ok {
            // Losing one match's rating is bad. Leaving the pod running is
            // worse, so the result is logged in full and the server still exits.
            eprintln!(
                "giving up on reporting, the result was: {}",
                serde_json::to_string(&body).unwrap_or_default()
            );
        }

        let _ = tx.send(ReportDone { ok });
    });
}

fn finish_shutdown(
    mut reporter: ResMut<Reporter>,
    session: Option<Res<Session>>,
    gf: Option<Res<GameFlowClient>>,
) {
    let mut done = false;
    while let Ok(msg) = reporter.rx.try_recv() {
        done = true;
        if msg.ok {
            info!("result accepted by the backend");
        }
    }

    if !done || reporter.winding_down {
        return;
    }
    reporter.winding_down = true;

    // Player tracking has to be left clean, otherwise the platform keeps
    // counting players that are gone.
    if let (Some(session), Some(gf)) = (session, gf.as_ref()) {
        for slot in session.slots.iter().filter(|s| s.connected) {
            gf.disconnect_player(slot.player_id.clone());
        }
    }

    if let Some(gf) = gf {
        info!("shutting down");
        gf.shutdown();
    }
}
