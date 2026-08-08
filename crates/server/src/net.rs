//! Transport and handshake.
//!
//! The socket cannot be bound until GameFlow tells us which port we were given,
//! so the server stays silent until `GameFlowConnected` fires, then binds,
//! then reports ready. Reporting ready before the socket exists would invite
//! players to connect to nothing.

use std::net::{SocketAddr, UdpSocket};
use std::time::SystemTime;

use bevy::prelude::*;
use bevy_renet::netcode::{
    NetcodeServerPlugin, NetcodeServerTransport, ServerAuthentication, ServerConfig,
};
use bevy_renet::{RenetServer, RenetServerPlugin};
use renet::{ChannelConfig, ConnectionConfig, SendType};

use pacman_shared::protocol::{CH_EVENT, CH_INPUT, CH_SNAPSHOT};

use crate::gameflow_plugin::{GameFlowClient, GameFlowConnected};
use crate::session::Session;
use crate::ServerConfigRes;

/// Distinguishes this game from anything else that might reach the port.
const PROTOCOL_ID: u64 = 0x5041_434D_414E_0001;
/// Fallback when the SDK reports no assigned port, which happens in local mode
/// unless `GAMEFLOW_DEFAULT_PORT` is set.
const FALLBACK_PORT: u16 = 2567;

const RESEND: std::time::Duration = std::time::Duration::from_millis(200);

/// Three channels, same shape in both directions.
///
/// Inputs and snapshots are unreliable on purpose: at 30Hz a lost packet is
/// replaced 33ms later, and retransmitting a stale world state is worse than
/// dropping it. Only the handshake and the final result must not be lost.
pub fn connection_config() -> ConnectionConfig {
    let channels = vec![
        ChannelConfig {
            channel_id: CH_INPUT,
            max_memory_usage_bytes: 64 * 1024,
            send_type: SendType::Unreliable,
        },
        ChannelConfig {
            channel_id: CH_SNAPSHOT,
            max_memory_usage_bytes: 256 * 1024,
            send_type: SendType::Unreliable,
        },
        ChannelConfig {
            channel_id: CH_EVENT,
            max_memory_usage_bytes: 256 * 1024,
            send_type: SendType::ReliableOrdered { resend_time: RESEND },
        },
    ];

    ConnectionConfig {
        available_bytes_per_tick: 60_000,
        server_channels_config: channels.clone(),
        client_channels_config: channels,
    }
}

pub struct NetServerPlugin;

impl Plugin for NetServerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((RenetServerPlugin, NetcodeServerPlugin))
            .add_observer(bind_socket);
    }
}

/// Binds the UDP socket on the port GameFlow assigned, then marks the server
/// ready so the platform starts sending players.
fn bind_socket(
    _: On<GameFlowConnected>,
    mut commands: Commands,
    client: Option<Res<GameFlowClient>>,
    config: Res<ServerConfigRes>,
) {
    let port = client
        .as_ref()
        .and_then(|c| c.default_port())
        .unwrap_or(FALLBACK_PORT);

    let addr: SocketAddr = format!("0.0.0.0:{port}").parse().expect("building bind address");

    let socket = match UdpSocket::bind(addr) {
        Ok(s) => s,
        Err(e) => {
            error!("could not bind {addr}: {e}");
            return;
        }
    };

    let public_addr = socket.local_addr().unwrap_or(addr);
    let server_config = ServerConfig {
        current_time: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock before the unix epoch"),
        max_clients: 2,
        protocol_id: PROTOCOL_ID,
        public_addresses: vec![public_addr],
        // The session token in the Hello message is what actually proves
        // identity, and it is signed with a secret the client never holds.
        // Netcode's own encryption would add a second key to distribute for no
        // extra guarantee.
        authentication: ServerAuthentication::Unsecure,
    };

    let transport = match NetcodeServerTransport::new(server_config, socket) {
        Ok(t) => t,
        Err(e) => {
            error!("could not start the netcode transport: {e}");
            return;
        }
    };

    commands.insert_resource(RenetServer::new(connection_config()));
    commands.insert_resource(transport);
    commands.insert_resource(Session::new(config.match_seed));

    info!("listening for players on udp/{port}");

    // Only now is it true that the server can accept players.
    if let Some(client) = client {
        client.ready();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_channel_id_is_configured_once() {
        let config = connection_config();
        let mut ids: Vec<u8> = config
            .server_channels_config
            .iter()
            .map(|c| c.channel_id)
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![CH_INPUT, CH_SNAPSHOT, CH_EVENT]);
    }

    #[test]
    fn both_directions_agree_on_channels() {
        let config = connection_config();
        let server: Vec<u8> = config
            .server_channels_config
            .iter()
            .map(|c| c.channel_id)
            .collect();
        let client: Vec<u8> = config
            .client_channels_config
            .iter()
            .map(|c| c.channel_id)
            .collect();
        assert_eq!(server, client, "a mismatch here silently drops messages");
    }

    #[test]
    fn only_the_event_channel_is_reliable() {
        for channel in connection_config().server_channels_config {
            let reliable = !matches!(channel.send_type, SendType::Unreliable);
            assert_eq!(
                reliable,
                channel.channel_id == CH_EVENT,
                "channel {} has the wrong delivery guarantee",
                channel.channel_id
            );
        }
    }
}
