//! Command-line interface for the native reference client.
//!
//! The flags are deliberately scriptable: an interop harness drives several of
//! these processes side by side and asserts global properties over their JSONL
//! stdout streams. Exactly one of `--create-room` / `--join-code` selects the
//! room mode; everything else has a sensible default.

use clap::{ArgGroup, Parser, ValueEnum};
use signal_fish_server::protocol::{Topology, Transport};

/// Native Rust reference client for the Signal Fish protocol v3.
///
/// Connects to a Signal Fish server over WebSocket, walks the full v3 flow
/// (Authenticate -> room -> ready barrier -> GameStarting -> SessionPlan), and
/// establishes real WebRTC peer connections (one reliable + one unreliable
/// data channel per peer) per the server-issued plan. stdout is a machine
/// interface: one JSON event object per line. All logging goes to stderr.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "signal-fish-reference-native",
    version,
    group(ArgGroup::new("room_mode").required(true).args(["create_room", "join_code"]))
)]
pub struct Cli {
    /// Full WebSocket URL of the signaling endpoint (e.g. ws://127.0.0.1:3536/v3/ws
    /// — 3536 is the server's default port).
    #[arg(long)]
    pub server_url: String,

    /// Create a new room (mutually exclusive with --join-code).
    #[arg(long)]
    pub create_room: bool,

    /// Join an existing room by its room code (mutually exclusive with --create-room).
    #[arg(long)]
    pub join_code: Option<String>,

    /// Expected total player count including this client; the client sends
    /// PlayerReady once it has observed this many room members. Also sets the
    /// room capacity (max_players) when this client creates the room.
    #[arg(long, default_value_t = 2)]
    pub peers: usize,

    /// Total DISTINCT session members (including this client) that must have
    /// been observed before this client may exit successfully; defaults to
    /// --peers. Late-join scenarios set this above --peers on the incumbents
    /// so they stay in the session until the late joiner has arrived (room
    /// capacity stays --peers: the server only finalizes full rooms, so a
    /// late join is always a seat fill after a departure).
    #[arg(long)]
    pub expect_total_peers: Option<usize>,

    /// Exit successfully as soon as GameStarting has been received, WITHOUT
    /// establishing any P2P pairs (a received SessionPlan is logged but not
    /// acted on, so no offers/answers/candidates are ever produced). Used by
    /// late-join harnesses to vacate a seat in a finalized room; not intended
    /// to be combined with --exchange or --relay-payload.
    #[arg(long)]
    pub leave_on_game_start: bool,

    /// Game name used when creating/joining the room.
    #[arg(long, default_value = "reference-native")]
    pub game_name: String,

    /// Player display name.
    #[arg(long, default_value = "RefNative")]
    pub player_name: String,

    /// App id sent in Authenticate (interop servers run with WebSocket auth disabled).
    #[arg(long, default_value = "reference-native-app")]
    pub app_id: String,

    /// Platform string reported in Authenticate.
    #[arg(long, default_value = "reference-native")]
    pub platform: String,

    /// When a P2P pair is fully open (both channels), send exactly one text
    /// message per channel and emit send/receive events.
    #[arg(long)]
    pub exchange: bool,

    /// After GameStarting (plus a short settle), send one GameData message with
    /// payload {"relay_msg": <text>} over the WebSocket relay floor.
    #[arg(long)]
    pub relay_payload: Option<String>,

    /// Deterministically cripple ICE: reject every network interface during
    /// candidate gathering AND silently drop all outbound/inbound IceCandidate
    /// signals. Used by fallback scenarios to force the relay floor.
    #[arg(long)]
    pub cripple_ice: bool,

    /// Seconds allowed for WebRTC pair establishment before the overall
    /// transport status resolves (true iff >= 1 pair connected at resolution).
    #[arg(long, default_value_t = 15)]
    pub p2p_timeout_secs: u64,

    /// Soft cap: exit nonzero if the flag-driven success criteria are still
    /// unmet after this many seconds.
    #[arg(long, default_value_t = 30)]
    pub run_for_secs: u64,

    /// Hard watchdog: abort with exit code 4 after this many seconds no matter
    /// what (the guarantee that this process can never hang a harness).
    #[arg(long, default_value_t = 60)]
    pub max_runtime_secs: u64,

    /// Protocol version to advertise in Authenticate. 2 omits every v3 field
    /// entirely (a pure v2 client for mixed-room tests).
    #[arg(long, default_value_t = 3)]
    pub protocol_version: u16,

    /// Comma-separated session topologies advertised in Authenticate (v3 only).
    #[arg(long, value_delimiter = ',', default_value = "relay,host,mesh")]
    pub supported_topologies: Vec<TopologyArg>,

    /// Comma-separated data-path transports advertised in Authenticate (v3 only).
    #[arg(long, value_delimiter = ',', default_value = "relay,webrtc")]
    pub supported_transports: Vec<TransportArg>,
}

/// CLI-parseable mirror of [`Topology`] (the protocol enum does not implement
/// `clap::ValueEnum`; wire tokens and CLI tokens are identical).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TopologyArg {
    Relay,
    Host,
    Mesh,
}

impl From<TopologyArg> for Topology {
    fn from(value: TopologyArg) -> Self {
        match value {
            TopologyArg::Relay => Topology::Relay,
            TopologyArg::Host => Topology::Host,
            TopologyArg::Mesh => Topology::Mesh,
        }
    }
}

/// CLI-parseable mirror of [`Transport`] (wire tokens and CLI tokens are
/// identical: `relay`, `direct`, `webrtc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TransportArg {
    Relay,
    Direct,
    Webrtc,
}

impl From<TransportArg> for Transport {
    fn from(value: TransportArg) -> Self {
        match value {
            TransportArg::Relay => Transport::Relay,
            TransportArg::Direct => Transport::Direct,
            TransportArg::Webrtc => Transport::WebRtc,
        }
    }
}

impl Cli {
    /// Topologies converted to the protocol enum, in CLI order.
    pub fn topologies(&self) -> Vec<Topology> {
        self.supported_topologies
            .iter()
            .map(|&topology| topology.into())
            .collect()
    }

    /// Transports converted to the protocol enum, in CLI order.
    pub fn transports(&self) -> Vec<Transport> {
        self.supported_transports
            .iter()
            .map(|&transport| transport.into())
            .collect()
    }

    /// Whether this run advertises protocol v3 (v2 omits all v3 fields).
    pub fn is_v3(&self) -> bool {
        self.protocol_version >= 3
    }

    /// Distinct session members (self included) required before a successful
    /// exit: `--expect-total-peers`, defaulting to `--peers` (where it is
    /// already implied by the ready barrier).
    pub fn effective_total_peers(&self) -> usize {
        self.expect_total_peers.unwrap_or(self.peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn create_room_mode_parses_with_defaults() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
        ]);
        assert!(cli.create_room);
        assert_eq!(cli.join_code, None);
        assert_eq!(cli.peers, 2);
        assert_eq!(cli.expect_total_peers, None);
        assert_eq!(
            cli.effective_total_peers(),
            2,
            "the member gate defaults to --peers"
        );
        assert!(!cli.leave_on_game_start);
        assert_eq!(cli.protocol_version, 3);
        assert!(cli.is_v3());
        assert_eq!(
            cli.topologies(),
            vec![Topology::Relay, Topology::Host, Topology::Mesh]
        );
        assert_eq!(cli.transports(), vec![Transport::Relay, Transport::WebRtc]);
    }

    #[test]
    fn join_mode_with_custom_capabilities_parses() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--join-code",
            "ABC123",
            "--peers",
            "3",
            "--supported-topologies",
            "relay,mesh",
            "--supported-transports",
            "relay,direct,webrtc",
            "--protocol-version",
            "2",
        ]);
        assert_eq!(cli.join_code.as_deref(), Some("ABC123"));
        assert_eq!(cli.peers, 3);
        assert!(!cli.is_v3());
        assert_eq!(cli.topologies(), vec![Topology::Relay, Topology::Mesh]);
        assert_eq!(
            cli.transports(),
            vec![Transport::Relay, Transport::Direct, Transport::WebRtc]
        );
    }

    #[test]
    fn late_join_flags_parse() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--peers",
            "3",
            "--expect-total-peers",
            "4",
            "--leave-on-game-start",
        ]);
        assert_eq!(cli.expect_total_peers, Some(4));
        assert_eq!(
            cli.effective_total_peers(),
            4,
            "--expect-total-peers overrides the --peers default"
        );
        assert!(cli.leave_on_game_start);
    }

    #[test]
    fn room_mode_is_required_and_exclusive() {
        let neither = Cli::try_parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
        ]);
        assert!(neither.is_err(), "one of create/join is required");

        let both = Cli::try_parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--join-code",
            "ABC123",
        ]);
        assert!(both.is_err(), "create and join are mutually exclusive");
    }
}
