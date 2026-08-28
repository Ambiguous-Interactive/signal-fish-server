//! Command-line interface for the native reference client.
//!
//! The flags are deliberately scriptable: an interop harness drives several of
//! these processes side by side and asserts global properties over their JSONL
//! stdout streams. Exactly one of `--create-room` / `--join-code` selects the
//! room mode; everything else has a sensible default.

use clap::{ArgGroup, Parser, ValueEnum};
use signal_fish_server::protocol::{Topology, Transport};
use std::path::PathBuf;

use crate::engine::{EngineSettings, IpFamily};

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
    /// PlayerReady once it has observed this many room members. Also the
    /// default room capacity (max_players) when this client creates the room.
    #[arg(long, default_value_t = 2)]
    pub peers: usize,

    /// Room capacity (`JoinRoom.max_players`) sent when this client CREATES a
    /// room; defaults to `--peers`, which keeps every scenario where the flag
    /// is absent byte-identical (rooms cap exactly at the expected party
    /// size). Values above `--peers` leave open seats after the room
    /// finalizes, which is the harness shape for live seat-fill scenarios: a
    /// late joiner fills a seat of the running session without any prior
    /// departure (issue #451). Joiners adopt the joined room's existing
    /// capacity, so the flag conflicts with `--join-code`. Must not sit below
    /// `--peers`: the room would fill before its members could ever reach the
    /// ready barrier.
    #[arg(long, conflicts_with = "join_code")]
    pub max_players: Option<u8>,

    /// Total DISTINCT session members (including this client) that must have
    /// been observed before this client may exit successfully; defaults to
    /// --peers. Late-join scenarios set this above --peers on the incumbents
    /// so they stay in the session until the late joiner has arrived.
    #[arg(long)]
    pub expect_total_peers: Option<usize>,

    /// Exit successfully after GameStarting and its authoritative SessionPlan,
    /// WITHOUT establishing any P2P pairs (the plan is logged but not acted
    /// on, so no offers/answers/candidates are ever produced). Used by
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

    /// Public app ID sent in Authenticate (interop servers use an open app-ID policy).
    #[arg(long, default_value = "reference-native-app")]
    pub app_id: String,

    /// Platform string reported in Authenticate.
    #[arg(long, default_value = "reference-native")]
    pub platform: String,

    /// When a P2P pair is fully open (both channels), send exactly one text
    /// message per channel and emit send/receive events.
    #[arg(long)]
    pub exchange: bool,

    /// Test-harness coordination: when --exchange is enabled, wait until this
    /// path exists before sending the exact per-channel exchange. Channel and
    /// pair establishment continue while held.
    #[arg(long, requires = "exchange")]
    pub exchange_release_file: Option<PathBuf>,

    /// Test-harness coordination for loss recovery: once this path exists,
    /// initiators rebuild every planned pair through the bounded PairRetry
    /// protocol before the held exchange is released.
    #[arg(long, requires = "exchange_release_file")]
    pub p2p_rebuild_release_file: Option<PathBuf>,

    /// Test-harness coordination for loss recovery: after the reliable half
    /// of --exchange has completed in both directions, wait until this path
    /// exists before sending the exact unreliable half.
    #[arg(long, requires = "exchange_release_file")]
    pub unreliable_exchange_release_file: Option<PathBuf>,

    /// After GameStarting (plus a short settle), send one GameData message with
    /// payload `{"relay_msg": "<text>"}` over the WebSocket relay floor.
    #[arg(long)]
    pub relay_payload: Option<String>,

    /// Deterministically cripple ICE: retain only a dummy loopback transport,
    /// register no usable local candidate, and silently drop all outbound and
    /// inbound IceCandidate signals. Used by fallback scenarios to force the
    /// relay floor while preserving SDP flow.
    #[arg(long)]
    pub cripple_ice: bool,

    /// ICE candidate policy for each peer connection. `relay` is a test and
    /// deployment diagnostic mode that permits TURN-relayed candidates only;
    /// `all` retains the normal direct-first behavior.
    #[arg(long, value_enum, default_value_t = IceTransportPolicyArg::All)]
    pub ice_transport_policy: IceTransportPolicyArg,

    /// TEST HARNESS ONLY: defer creation of every planned peer connection
    /// until this path exists. WebSocket processing and relay traffic continue
    /// while held.
    #[arg(long)]
    pub p2p_release_file: Option<PathBuf>,

    /// TEST HARNESS ONLY: disable multicast-DNS candidate resolution. Native
    /// host candidates remain raw IPs in either mode; this keeps remote `.local`
    /// discovery traffic out of packet-loss experiments.
    #[arg(long)]
    pub disable_mdns: bool,

    /// Restrict the ICE sockets this client binds to one address family, and
    /// therefore the family of every host candidate it advertises. `any` (the
    /// default) binds every usable interface address. An explicitly requested
    /// family is resolved before the client opens its WebSocket: a host that
    /// cannot serve it fails the process — without creating or joining a room
    /// — instead of silently negotiating the other family or degrading to the
    /// relay floor. `--cripple-ice` is exempt: that transport is a deliberate
    /// dead end and always binds the IPv4 loopback.
    #[arg(long, value_enum, default_value_t = IpFamily::Any)]
    pub ip_family: IpFamily,

    /// FAULT INJECTION (matrix harness only): discard inbound trickle-ICE
    /// candidates from the planned peer named `cNN`, where NN is this ordinal.
    /// Offer/answer signaling, other peer links, and the relay floor stay live.
    #[arg(long)]
    pub drop_ice_from: Option<usize>,

    /// Seconds allowed for WebRTC pair establishment before the overall
    /// transport status resolves (true iff >= 1 pair connected at resolution).
    #[arg(long, default_value_t = 15)]
    pub p2p_timeout_secs: u64,

    /// Maximum coordinated rebuilds for a planned pair whose data channels do
    /// not open. Retries use the server's opaque Signal relay and preserve the
    /// plan's glare role. Fault-injection tests that require a permanently
    /// missing pair set this to 0.
    #[arg(long, default_value_t = 0)]
    pub p2p_retry_count: u8,

    /// Soft cap: exit nonzero if the flag-driven success criteria are still
    /// unmet after this many seconds.
    #[arg(long, default_value_t = 30)]
    pub run_for_secs: u64,

    /// Watchdog: abort with exit code 4 after this many seconds when the
    /// absolute deadline is representable. Larger accepted values remain
    /// beyond the process lifetime.
    #[arg(long, default_value_t = 60)]
    pub max_runtime_secs: u64,

    /// Test-harness coordination: after all success criteria are met, emit
    /// `success_criteria_met` and keep the connection/pairs alive until this
    /// path exists. Normal runs omit this flag and retain immediate bounded
    /// success exit behavior.
    #[arg(long)]
    pub success_release_file: Option<PathBuf>,

    /// Test-harness signal-ledger coordination: include completion of ICE
    /// gathering for every live peer-connection generation in the success
    /// criteria. This is deliberately separate from `--success-release-file`:
    /// a connected selected path can carry gameplay while unrelated candidate
    /// transactions are still settling.
    #[arg(long, requires = "success_release_file")]
    pub require_ice_gathering_complete: bool,

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

    /// Tokio runtime flavor driving the whole process. `multi` (the default)
    /// is the multi-threaded runtime; `current` runs everything on a single
    /// current-thread runtime — the shape most susceptible to being starved by
    /// a blocking game loop (see --tick-stall-ms), which the starved-runtime
    /// conformance matrix pins as an executable boundary.
    #[arg(long, value_enum, default_value_t = RuntimeFlavor::Multi)]
    pub runtime: RuntimeFlavor,

    /// FAULT INJECTION: block the orchestrator's executor thread for this many
    /// milliseconds after each processed input (`std::thread::sleep`, NOT an
    /// async sleep — the point is to deliberately hog the runtime), simulating
    /// a game loop that "ticks" its networking occasionally instead of
    /// continuously driving it. Exists solely for conformance-testing the
    /// server's slow-consumer contract and the documented "clients driving
    /// async runtimes must continuously poll/drive their connection"
    /// requirement (docs/protocol.md, "Delivery reliability and backpressure").
    /// 0 (the default) disables the stall.
    #[arg(long, default_value_t = 0)]
    pub tick_stall_ms: u64,
}

/// CLI token for the tokio runtime flavor (see [`Cli::runtime`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RuntimeFlavor {
    /// The default multi-threaded runtime (`tokio::runtime::Runtime::new`).
    Multi,
    /// A single current-thread runtime: every task shares one executor
    /// thread, so a blocking stall (--tick-stall-ms) starves the whole client.
    Current,
}

/// CLI mirror of WebRTC's ICE transport policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IceTransportPolicyArg {
    All,
    Relay,
}

impl IceTransportPolicyArg {
    /// Whether only TURN relay candidates may be used.
    pub fn is_relay_only(self) -> bool {
        self == Self::Relay
    }
}

impl RuntimeFlavor {
    /// Stable lowercase token for event output (matches the CLI token).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Multi => "multi",
            Self::Current => "current",
        }
    }
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

    /// The WebRTC engine configuration this invocation asks for.
    pub fn engine_settings(&self) -> EngineSettings {
        EngineSettings {
            crippled: self.cripple_ice,
            disable_mdns: self.disable_mdns,
            relay_only: self.ice_transport_policy.is_relay_only(),
            ip_family: self.ip_family,
        }
    }

    /// Distinct session members (self included) required before a successful
    /// exit: `--expect-total-peers`, defaulting to `--peers` (where it is
    /// already implied by the ready barrier).
    pub fn effective_total_peers(&self) -> usize {
        self.expect_total_peers.unwrap_or(self.peers)
    }

    /// The `max_players` value sent in `JoinRoom`: an explicit `--max-players`
    /// (creator only), else `--peers`. The protocol carries capacity as u8, so
    /// a `--peers`-derived value above 255 is a usage error, a capacity below
    /// one reaches no one, and a creator capacity below `--peers` is rejected
    /// because the ready barrier could never be reached.
    pub fn join_max_players(&self) -> Result<u8, String> {
        if self.peers < 1 {
            return Err(format!("--peers must be at least 1, got {}", self.peers));
        }
        let max_players = self
            .max_players
            .or_else(|| u8::try_from(self.peers).ok())
            .ok_or_else(|| {
                if self.create_room {
                    format!(
                        "--peers {} exceeds the u8 room-capacity type; pass --max-players or \
                         lower --peers",
                        self.peers
                    )
                } else {
                    format!("--peers {} exceeds the u8 room-capacity type", self.peers)
                }
            })?;
        if max_players < 1 {
            return Err(format!(
                "room capacity must be at least 1, got {max_players}"
            ));
        }
        if self.create_room && usize::from(max_players) < self.peers {
            return Err(format!(
                "--max-players {max_players} is below the --peers {} ready barrier: the room \
                 would fill before its members could ever ready up",
                self.peers
            ));
        }
        Ok(max_players)
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
        assert_eq!(cli.max_players, None);
        assert_eq!(
            cli.join_max_players(),
            Ok(2),
            "room capacity defaults to --peers"
        );
        assert_eq!(cli.expect_total_peers, None);
        assert_eq!(
            cli.effective_total_peers(),
            2,
            "the member gate defaults to --peers"
        );
        assert!(!cli.leave_on_game_start);
        assert_eq!(cli.protocol_version, 3);
        assert!(cli.is_v3());
        assert_eq!(cli.runtime, RuntimeFlavor::Multi);
        assert_eq!(cli.runtime.as_str(), "multi");
        assert_eq!(cli.tick_stall_ms, 0, "fault injection is off by default");
        assert_eq!(cli.p2p_retry_count, 0, "pair retry is opt-in");
        assert_eq!(cli.drop_ice_from, None);
        assert!(!cli.disable_mdns);
        assert_eq!(cli.ice_transport_policy, IceTransportPolicyArg::All);
        assert_eq!(cli.p2p_release_file, None);
        assert_eq!(cli.success_release_file, None);
        assert!(!cli.require_ice_gathering_complete);
        assert_eq!(cli.exchange_release_file, None);
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
    fn max_players_defaults_to_peers_and_overrides_when_given() {
        let defaulted = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--peers",
            "2",
        ]);
        assert_eq!(
            defaulted.join_max_players(),
            Ok(2),
            "without --max-players the room caps exactly at the party size"
        );

        let oversized_creator = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--peers",
            "300",
        ]);
        assert_eq!(
            oversized_creator.join_max_players(),
            Err(
                "--peers 300 exceeds the u8 room-capacity type; pass --max-players or lower \
                 --peers"
                    .to_string()
            ),
            "the creator branch names the creator-only remedy"
        );
        let oversized_joiner = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--join-code",
            "ABC123",
            "--peers",
            "300",
        ]);
        let joiner_error = oversized_joiner
            .join_max_players()
            .expect_err("300 exceeds the u8 capacity type");
        assert!(
            !joiner_error.contains("--max-players"),
            "the joiner branch must not suggest the creator-only flag: {joiner_error}"
        );

        let open = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--peers",
            "2",
            "--max-players",
            "3",
        ]);
        assert_eq!(
            open.join_max_players(),
            Ok(3),
            "--max-players raises the capacity above the ready barrier (issue #451)"
        );

        assert!(
            Cli::try_parse_from([
                "signal-fish-reference-native",
                "--server-url",
                "ws://127.0.0.1:9000/v3/ws",
                "--create-room",
                "--max-players",
                "256",
            ])
            .is_err(),
            "capacity is a u8 on the wire; 256 must be rejected by the parser"
        );

        let joiner = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--join-code",
            "ABC123",
        ]);
        assert_eq!(
            joiner.join_max_players(),
            Ok(2),
            "a joiner still sends a u8 capacity (the server keeps the room's own)"
        );

        let zero_peers = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--join-code",
            "ABC123",
            "--peers",
            "0",
        ]);
        assert_eq!(
            zero_peers.join_max_players(),
            Err("--peers must be at least 1, got 0".to_string()),
            "a zero ready barrier reaches no one (browser parse-time parity)"
        );
        let zero_peers_creator = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--peers",
            "0",
            "--max-players",
            "3",
        ]);
        assert_eq!(
            zero_peers_creator.join_max_players(),
            Err("--peers must be at least 1, got 0".to_string()),
            "an explicit capacity does not rescue a degenerate --peers 0"
        );
        let zero_capacity = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--max-players",
            "0",
        ]);
        assert_eq!(
            zero_capacity.join_max_players(),
            Err("room capacity must be at least 1, got 0".to_string()),
            "an explicit zero capacity reaches no one"
        );
        assert!(
            Cli::try_parse_from([
                "signal-fish-reference-native",
                "--server-url",
                "ws://127.0.0.1:9000/v3/ws",
                "--join-code",
                "ABC123",
                "--max-players",
                "3",
            ])
            .is_err(),
            "joiners adopt the joined room's capacity; --max-players must require --create-room"
        );
    }

    #[test]
    fn creator_capacity_below_the_ready_barrier_is_rejected() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--peers",
            "3",
            "--max-players",
            "2",
        ]);
        let error = cli
            .join_max_players()
            .expect_err("a room that fills before its barrier must be a usage error");
        assert!(
            error.contains("--max-players 2"),
            "the error names the capacity: {error}"
        );

        // A joiner's capacity is advisory (the server keeps the room's own),
        // so the same numbers stay valid in join mode.
        let joiner = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--join-code",
            "ABC123",
            "--peers",
            "3",
        ]);
        assert_eq!(joiner.join_max_players(), Ok(3));
    }

    #[test]
    fn starved_runtime_flags_parse() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--runtime",
            "current",
            "--tick-stall-ms",
            "750",
        ]);
        assert_eq!(cli.runtime, RuntimeFlavor::Current);
        assert_eq!(cli.runtime.as_str(), "current");
        assert_eq!(cli.tick_stall_ms, 750);
    }

    #[test]
    fn per_peer_ice_fault_target_ordinal_parses() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--drop-ice-from",
            "12",
        ]);
        assert_eq!(cli.drop_ice_from, Some(12));
    }

    #[test]
    fn harness_can_disable_mdns_candidate_obfuscation() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--disable-mdns",
        ]);
        assert!(cli.disable_mdns);
    }

    #[test]
    fn ip_family_selection_reaches_the_engine_settings() {
        let default = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
        ]);
        assert_eq!(default.ip_family, IpFamily::Any);
        assert_eq!(
            default.engine_settings(),
            EngineSettings::default(),
            "a plain run must configure the engine's production defaults"
        );

        let ipv6 = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://[::1]:9000/v3/ws",
            "--create-room",
            "--ip-family",
            "ipv6",
            "--disable-mdns",
            "--cripple-ice",
            "--ice-transport-policy",
            "relay",
        ]);
        assert_eq!(
            ipv6.engine_settings(),
            EngineSettings {
                crippled: true,
                disable_mdns: true,
                relay_only: true,
                ip_family: IpFamily::Ipv6,
            }
        );

        assert!(
            Cli::try_parse_from([
                "signal-fish-reference-native",
                "--server-url",
                "ws://127.0.0.1:9000/v3/ws",
                "--create-room",
                "--ip-family",
                "ipv5",
            ])
            .is_err(),
            "an unknown address family must be rejected, not defaulted"
        );
    }

    #[test]
    fn relay_only_ice_transport_policy_parses() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--ice-transport-policy",
            "relay",
        ]);
        assert!(cli.ice_transport_policy.is_relay_only());
    }

    #[test]
    fn p2p_release_file_parses() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--p2p-release-file",
            "/tmp/signal-fish-p2p-release",
        ]);
        assert_eq!(
            cli.p2p_release_file.as_deref(),
            Some(std::path::Path::new("/tmp/signal-fish-p2p-release"))
        );
    }

    #[test]
    fn success_release_file_parses() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--success-release-file",
            "/tmp/signal-fish-release",
            "--require-ice-gathering-complete",
        ]);
        assert_eq!(
            cli.success_release_file.as_deref(),
            Some(std::path::Path::new("/tmp/signal-fish-release"))
        );
        assert!(cli.require_ice_gathering_complete);

        let without_release = Cli::try_parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--require-ice-gathering-complete",
        ]);
        assert!(
            without_release.is_err(),
            "the gather-complete success criterion requires a held success barrier"
        );
    }

    #[test]
    fn exchange_release_file_parses() {
        let cli = Cli::parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--exchange",
            "--exchange-release-file",
            "/tmp/signal-fish-exchange-release",
            "--p2p-rebuild-release-file",
            "/tmp/signal-fish-p2p-rebuild",
            "--p2p-retry-count",
            "1",
            "--unreliable-exchange-release-file",
            "/tmp/signal-fish-unreliable-release",
        ]);
        assert!(cli.exchange);
        assert_eq!(
            cli.exchange_release_file.as_deref(),
            Some(std::path::Path::new("/tmp/signal-fish-exchange-release"))
        );
        assert_eq!(
            cli.unreliable_exchange_release_file.as_deref(),
            Some(std::path::Path::new("/tmp/signal-fish-unreliable-release"))
        );
        assert_eq!(
            cli.p2p_rebuild_release_file.as_deref(),
            Some(std::path::Path::new("/tmp/signal-fish-p2p-rebuild"))
        );

        let without_exchange = Cli::try_parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--exchange-release-file",
            "/tmp/signal-fish-exchange-release",
        ]);
        assert!(
            without_exchange.is_err(),
            "the exchange gate is meaningless without --exchange"
        );

        let without_reliable_gate = Cli::try_parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--exchange",
            "--unreliable-exchange-release-file",
            "/tmp/signal-fish-unreliable-release",
        ]);
        assert!(
            without_reliable_gate.is_err(),
            "the unreliable gate requires the reliable exchange gate"
        );

        let rebuild_without_exchange_gate = Cli::try_parse_from([
            "signal-fish-reference-native",
            "--server-url",
            "ws://127.0.0.1:9000/v3/ws",
            "--create-room",
            "--exchange",
            "--p2p-rebuild-release-file",
            "/tmp/signal-fish-p2p-rebuild",
            "--p2p-retry-count",
            "1",
        ]);
        assert!(
            rebuild_without_exchange_gate.is_err(),
            "the P2P rebuild gate requires the held exchange gate"
        );
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
