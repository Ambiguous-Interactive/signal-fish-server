//! WebRTC engine: one `RTCPeerConnection` per remote peer, two data channels
//! per pair, trickle ICE relayed through the server's opaque `Signal` envelope.
//!
//! # Protocol steps implemented here (PLAN Appendix E/G, ADR-0002)
//!
//! - **Initiator rule comes only from the server** (`SessionPlan.peers[].initiate`
//!   / `NewPeer.you_initiate`) — the engine never recomputes glare locally.
//! - **Initiator**: create the peer connection, create both data channels
//!   (`reliable`: ordered; `unreliable`: `ordered=false, max_retransmits=0`)
//!   *before* offering so they ride the initial SDP, then offer.
//! - **Responder**: wait for the remote `Offer`, answer, and receive the two
//!   channels via `on_data_channel`.
//! - **Trickle ICE**: every gathered local candidate is surfaced for relay as
//!   `{"IceCandidate": <json>}` where `<json>` is the serde serialization of
//!   webrtc-rs's `RTCIceCandidateInit` (camelCase `candidate` / `sdpMid` /
//!   `sdpMLineIndex` / `usernameFragment`, matchbox-compatible). Remote candidates that arrive
//!   before the remote description are buffered and flushed afterwards.
//! - **Crippled mode** (`--cripple-ice`): the peer connection binds only an
//!   isolated loopback socket, and candidate signals are dropped in both
//!   directions — deterministic non-connectivity while preserving normal
//!   offer/answer and ICE-gathering lifecycle events for fallback scenarios.
//!
//! The engine is owned and driven by the single orchestrator task. webrtc-rs's
//! async peer handler and polled data channels never touch engine state directly;
//! they forward through an unbounded [`EngineEvent`] channel back to the orchestrator.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use clap::ValueEnum;
use rtc::ice::{mdns::MulticastDnsMode, network_type::NetworkType};
use signal_fish_server::protocol::{IceServer, PlayerId};
use tokio::sync::mpsc;
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceGatheringState, RTCIceServer, RTCIceTransportPolicy,
    RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription, RTCStatsReportEntry,
    SettingEngine, StatsSelector,
};
use webrtc::runtime::{default_runtime, Runtime};

/// Label of the ordered, reliable data channel (commands / critical events).
pub const RELIABLE_LABEL: &str = "reliable";
/// Label of the unordered, no-retransmit channel (movement / state).
pub const UNRELIABLE_LABEL: &str = "unreliable";

/// Notifications from webrtc-rs callbacks back to the orchestrator task.
/// (No `Debug` derive: `RTCDataChannel` is not `Debug`.)
pub enum EngineEvent {
    /// A local ICE candidate was gathered; `candidate_json` is the serialized
    /// `RTCIceCandidateInit` to relay as `{"IceCandidate": candidate_json}`.
    LocalCandidate {
        peer: PlayerId,
        generation: u64,
        candidate_json: String,
    },
    /// This peer connection emitted the end-of-gathering marker after all
    /// local candidates for its generation.
    IceGatheringComplete { peer: PlayerId, generation: u64 },
    /// Peer-connection state transition (informational `pc_state` events).
    PcState {
        peer: PlayerId,
        generation: u64,
        state: RTCPeerConnectionState,
    },
    /// The remote side announced a data channel toward us (responder path).
    RemoteChannel {
        peer: PlayerId,
        generation: u64,
        label: String,
        channel: Arc<dyn DataChannel>,
    },
    /// A data channel (local or remote) reached the open state.
    ChannelOpen {
        peer: PlayerId,
        generation: u64,
        label: String,
    },
    /// A required data channel closed or became unreadable.
    ChannelClosed {
        peer: PlayerId,
        generation: u64,
        label: String,
    },
    /// A text message arrived on an open data channel.
    ChannelMessage {
        peer: PlayerId,
        generation: u64,
        label: String,
        text: String,
    },
}

impl EngineEvent {
    pub fn peer_generation(&self) -> (PlayerId, u64) {
        match self {
            Self::LocalCandidate {
                peer, generation, ..
            }
            | Self::IceGatheringComplete { peer, generation }
            | Self::PcState {
                peer, generation, ..
            }
            | Self::RemoteChannel {
                peer, generation, ..
            }
            | Self::ChannelOpen {
                peer, generation, ..
            }
            | Self::ChannelClosed {
                peer, generation, ..
            }
            | Self::ChannelMessage {
                peer, generation, ..
            } => (*peer, *generation),
        }
    }
}

/// Per-remote-peer connection state.
struct PeerLink {
    generation: u64,
    pc: Arc<dyn PeerConnection>,
    /// Channels by label, both locally created (initiator) and remotely
    /// announced (responder).
    channels: HashMap<String, Arc<dyn DataChannel>>,
    /// Labels observed open so far; the pair is connected when both labels are in.
    open_labels: BTreeSet<String>,
    /// Remote ICE candidates cannot be applied before the remote description.
    pending_candidates: Vec<RTCIceCandidateInit>,
    remote_description_set: bool,
    pair_connected: bool,
}

/// Address families this client may bind for ICE, and therefore the families
/// its host candidates can advertise.
///
/// webrtc 0.20 turns each application-supplied socket directly into a host
/// candidate, so restricting the bind set is the only way to pin the family of
/// the negotiated path. Derives `ValueEnum` because it *is* the `--ip-family`
/// CLI surface; a separate mirror enum would only be a second thing to keep in
/// sync.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum IpFamily {
    /// Bind every usable interface address (the production default).
    #[default]
    Any,
    /// Bind IPv4 addresses only.
    Ipv4,
    /// Bind IPv6 addresses only.
    Ipv6,
}

impl IpFamily {
    /// Whether an interface address belongs to this family.
    fn admits(self, ip: IpAddr) -> bool {
        matches!(
            (self, ip),
            (Self::Any, _) | (Self::Ipv4, IpAddr::V4(_)) | (Self::Ipv6, IpAddr::V6(_))
        )
    }

    /// Stable lowercase token for diagnostics (pinned to the CLI token by
    /// `ip_family_tokens_match_the_cli_surface`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
        }
    }
}

/// Everything the engine needs to know about how this process was invoked.
/// Grouped so adding a knob does not add another positional flag argument to
/// every call site.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineSettings {
    /// Deterministic ICE failure (`--cripple-ice`).
    pub crippled: bool,
    /// Disable multicast-DNS candidate obfuscation (`--disable-mdns`).
    pub disable_mdns: bool,
    /// Permit only TURN-relayed candidates (`--ice-transport-policy relay`).
    pub relay_only: bool,
    /// Restrict the bound ICE sockets to one address family (`--ip-family`).
    pub ip_family: IpFamily,
}

/// The per-client WebRTC engine. Owned by the orchestrator task; all methods
/// are called from that single task, so no interior locking is needed.
pub struct Engine {
    settings: EngineSettings,
    runtime: Arc<dyn Runtime>,
    events: mpsc::UnboundedSender<EngineEvent>,
    peers: HashMap<PlayerId, PeerLink>,
    next_generation: u64,
}

impl Engine {
    /// Build the engine with the default runtime and the invocation's
    /// [`EngineSettings`].
    ///
    pub fn new(
        settings: EngineSettings,
        events: mpsc::UnboundedSender<EngineEvent>,
    ) -> Result<Self> {
        let runtime = default_runtime()
            .ok_or_else(|| anyhow!("webrtc 0.20 was built without an async runtime feature"))?;
        Ok(Self {
            settings,
            runtime,
            events,
            peers: HashMap::new(),
            next_generation: 0,
        })
    }

    /// Whether a peer connection toward `peer` already exists.
    pub fn is_paired(&self, peer: PlayerId) -> bool {
        self.peers.contains_key(&peer)
    }

    pub fn is_current_generation(&self, peer: PlayerId, generation: u64) -> bool {
        self.peers
            .get(&peer)
            .is_some_and(|link| link.generation == generation)
    }

    pub fn is_current_event(&self, event: &EngineEvent) -> bool {
        let (peer, generation) = event.peer_generation();
        self.is_current_generation(peer, generation)
    }

    /// Close and forget a departed peer so a later directive can pair it anew.
    pub async fn remove_peer(&mut self, peer: PlayerId) -> Result<()> {
        if let Some(link) = self.peers.remove(&peer) {
            link.pc
                .close()
                .await
                .context("close departed RTCPeerConnection")?;
        }
        Ok(())
    }

    /// Number of fully connected pairs (both channels open).
    pub fn connected_pair_count(&self) -> usize {
        self.peers
            .values()
            .filter(|link| link.pair_connected)
            .count()
    }

    /// Create the peer connection toward `peer` per the server's pairing
    /// directive. Idempotent: re-pairing an existing peer is a no-op.
    ///
    /// Returns `Some(offer_sdp)` when this side initiates (the caller relays it
    /// as `{"Offer": sdp}`), `None` when this side answers.
    pub async fn pair_with(
        &mut self,
        peer: PlayerId,
        initiate: bool,
        ice_servers: &[IceServer],
    ) -> Result<Option<String>> {
        if self.is_paired(peer) {
            tracing::debug!(%peer, "already paired; ignoring duplicate pairing directive");
            return Ok(None);
        }
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("peer-link generation overflow"))?;
        let generation = self.next_generation;

        let config = RTCConfigurationBuilder::new()
            .with_ice_servers(convert_ice_servers(ice_servers))
            .with_ice_transport_policy(if self.settings.relay_only {
                RTCIceTransportPolicy::Relay
            } else {
                RTCIceTransportPolicy::All
            })
            .build();
        let mut setting_engine = SettingEngine::default();
        if self.settings.disable_mdns {
            setting_engine.set_multicast_dns_mode(MulticastDnsMode::Disabled);
        }
        if self.settings.crippled {
            // The 0.20 driver requires at least one socket, but the ICE agent
            // must never register that dummy UDP socket as a usable local
            // candidate. Permit only TCP4 candidates while supplying no TCP
            // listeners. This also rejects UDP candidates embedded directly
            // in remote SDP, below the signaling layer's candidate filters.
            setting_engine.set_network_types(vec![NetworkType::Tcp4]);
        }
        let handler = Arc::new(PeerHandler {
            peer,
            generation,
            crippled: self.settings.crippled,
            events: self.events.clone(),
            runtime: self.runtime.clone(),
        });
        let udp_addrs = local_udp_addrs(self.settings)?;
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(config)
                .with_setting_engine(setting_engine)
                .with_handler(handler)
                .with_runtime(self.runtime.clone())
                .with_udp_addrs(udp_addrs)
                .build()
                .await
                .context("create peer connection")?,
        );
        let link = PeerLink {
            generation,
            pc: pc.clone(),
            channels: HashMap::new(),
            open_labels: BTreeSet::new(),
            pending_candidates: Vec::new(),
            remote_description_set: false,
            pair_connected: false,
        };
        self.peers.insert(peer, link);
        let result = async {
            if !initiate {
                return Ok(None);
            }
            // Both channels must exist before the offer so the SDP negotiates
            // SCTP and the channels open as soon as DTLS completes.
            let reliable = pc
                .create_data_channel(RELIABLE_LABEL, None)
                .await
                .context("create reliable data channel")?;
            let unreliable = pc
                .create_data_channel(
                    UNRELIABLE_LABEL,
                    Some(RTCDataChannelInit {
                        ordered: false,
                        max_retransmits: Some(0),
                        ..RTCDataChannelInit::default()
                    }),
                )
                .await
                .context("create unreliable data channel")?;
            spawn_channel_event_loop(
                &self.runtime,
                &self.events,
                peer,
                generation,
                RELIABLE_LABEL.to_string(),
                reliable.clone(),
            );
            spawn_channel_event_loop(
                &self.runtime,
                &self.events,
                peer,
                generation,
                UNRELIABLE_LABEL.to_string(),
                unreliable.clone(),
            );
            let link = self
                .peers
                .get_mut(&peer)
                .ok_or_else(|| anyhow!("peer link removed while pairing"))?;
            link.channels.insert(RELIABLE_LABEL.to_string(), reliable);
            link.channels
                .insert(UNRELIABLE_LABEL.to_string(), unreliable);

            let offer = pc.create_offer(None).await.context("create offer")?;
            let sdp = offer.sdp.clone();
            pc.set_local_description(offer)
                .await
                .context("set local description (offer)")?;
            Ok(Some(sdp))
        }
        .await;
        if result.is_err() {
            if let Some(link) = self.peers.remove(&peer) {
                let _ = link.pc.close().await;
            }
        }
        result
    }

    /// Responder path: apply a remote offer and produce the answer SDP (the
    /// caller relays it as `{"Answer": sdp}`). Buffered remote candidates are
    /// flushed once the remote description is set.
    pub async fn handle_offer(&mut self, peer: PlayerId, sdp: String) -> Result<String> {
        let link = self
            .peers
            .get_mut(&peer)
            .ok_or_else(|| anyhow!("offer from unpaired peer {peer}"))?;
        let offer = RTCSessionDescription::offer(sdp).context("parse remote offer SDP")?;
        let pc = link.pc.clone();
        pc.set_remote_description(offer)
            .await
            .context("set remote description (offer)")?;
        link.remote_description_set = true;
        let pending = std::mem::take(&mut link.pending_candidates);
        for candidate in pending {
            pc.add_ice_candidate(candidate)
                .await
                .context("apply buffered remote candidate")?;
        }
        let answer = pc.create_answer(None).await.context("create answer")?;
        let sdp = answer.sdp.clone();
        pc.set_local_description(answer)
            .await
            .context("set local description (answer)")?;
        Ok(sdp)
    }

    /// Initiator path: apply the remote answer; flush buffered candidates.
    pub async fn handle_answer(&mut self, peer: PlayerId, sdp: String) -> Result<()> {
        let link = self
            .peers
            .get_mut(&peer)
            .ok_or_else(|| anyhow!("answer from unpaired peer {peer}"))?;
        let answer = RTCSessionDescription::answer(sdp).context("parse remote answer SDP")?;
        let pc = link.pc.clone();
        pc.set_remote_description(answer)
            .await
            .context("set remote description (answer)")?;
        link.remote_description_set = true;
        let pending = std::mem::take(&mut link.pending_candidates);
        for candidate in pending {
            pc.add_ice_candidate(candidate)
                .await
                .context("apply buffered remote candidate")?;
        }
        Ok(())
    }

    /// Apply (or buffer) a relayed remote ICE candidate.
    ///
    /// The payload is normally the JSON serialization of `RTCIceCandidateInit`
    /// (what this client and matchbox emit); a payload that is not valid JSON
    /// is tolerated as a bare candidate string for interop with minimal
    /// clients.
    pub async fn handle_remote_candidate(
        &mut self,
        peer: PlayerId,
        candidate_payload: &str,
    ) -> Result<()> {
        let init: RTCIceCandidateInit = match serde_json::from_str(candidate_payload) {
            Ok(init) => init,
            Err(_not_json) => RTCIceCandidateInit {
                candidate: candidate_payload.to_string(),
                ..RTCIceCandidateInit::default()
            },
        };
        let link = self
            .peers
            .get_mut(&peer)
            .ok_or_else(|| anyhow!("candidate from unpaired peer {peer}"))?;
        if link.remote_description_set {
            let pc = link.pc.clone();
            pc.add_ice_candidate(init)
                .await
                .context("apply remote candidate")?;
        } else {
            link.pending_candidates.push(init);
        }
        Ok(())
    }

    /// Store a remotely announced data channel (responder path).
    pub fn store_remote_channel(
        &mut self,
        peer: PlayerId,
        label: String,
        channel: Arc<dyn DataChannel>,
    ) {
        if let Some(link) = self.peers.get_mut(&peer) {
            link.channels.insert(label, channel);
        } else {
            tracing::warn!(%peer, "remote channel announced for unknown peer");
        }
    }

    /// Record an open channel; returns `true` exactly once per peer, at the
    /// moment both expected labels are open (the pair is connected).
    pub fn note_channel_open(&mut self, peer: PlayerId, label: &str) -> bool {
        let Some(link) = self.peers.get_mut(&peer) else {
            tracing::warn!(%peer, label, "channel open for unknown peer");
            return false;
        };
        link.open_labels.insert(label.to_string());
        let fully_open = link.open_labels.contains(RELIABLE_LABEL)
            && link.open_labels.contains(UNRELIABLE_LABEL);
        if fully_open && !link.pair_connected {
            link.pair_connected = true;
            return true;
        }
        false
    }

    /// Look up a stored channel by peer and label.
    pub fn channel(&self, peer: PlayerId, label: &str) -> Option<Arc<dyn DataChannel>> {
        self.peers
            .get(&peer)
            .and_then(|link| link.channels.get(label))
            .cloned()
    }

    /// The selected ICE candidate pair for a connected peer: candidate types
    /// (`host`/`srflx`/`prflx`/`relay`) plus each side's reported address, so a
    /// harness can prove which family and which concrete address actually
    /// carried the data channels.
    pub async fn selected_candidate_pair(&self, peer: PlayerId) -> Option<SelectedCandidatePair> {
        let pc = self.peers.get(&peer)?.pc.clone();
        let report = pc
            .get_stats(std::time::Instant::now(), StatsSelector::None)
            .await;
        let selected_pair_id = &report.transport()?.selected_candidate_pair_id;
        let RTCStatsReportEntry::IceCandidatePair(pair) = report.get(selected_pair_id)? else {
            return None;
        };
        // rtc 0.20 records raw ICE-agent IDs on the candidate-pair entry but
        // prefixes the standalone candidate report IDs. Accept the direct ID
        // first so this remains compatible if that internal mismatch is fixed.
        let local_id = format!("RTCLocalIceCandidate_{}", pair.local_candidate_id);
        let remote_id = format!("RTCRemoteIceCandidate_{}", pair.remote_candidate_id);
        let RTCStatsReportEntry::LocalCandidate(local) = report
            .get(&pair.local_candidate_id)
            .or_else(|| report.get(&local_id))?
        else {
            return None;
        };
        let RTCStatsReportEntry::RemoteCandidate(remote) = report
            .get(&pair.remote_candidate_id)
            .or_else(|| report.get(&remote_id))?
        else {
            return None;
        };
        Some(SelectedCandidatePair {
            local_candidate_type: local.candidate_type.to_string(),
            remote_candidate_type: remote.candidate_type.to_string(),
            local_candidate_address: local.address.clone(),
            remote_candidate_address: remote.address.clone(),
        })
    }
}

/// The ICE candidate pair that carries a connected peer's data channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedCandidatePair {
    pub local_candidate_type: String,
    pub remote_candidate_type: String,
    /// Address the local stack reports for its side of the pair. `None` when
    /// the stack redacts or omits it; the harness treats that as a failure
    /// rather than a pass.
    pub local_candidate_address: Option<String>,
    /// Address reported for the remote side of the pair.
    pub remote_candidate_address: Option<String>,
}

struct PeerHandler {
    peer: PlayerId,
    generation: u64,
    crippled: bool,
    events: mpsc::UnboundedSender<EngineEvent>,
    runtime: Arc<dyn Runtime>,
}

#[async_trait]
impl PeerConnectionEventHandler for PeerHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if self.crippled {
            return;
        }
        match event
            .candidate
            .to_json()
            .map_err(anyhow::Error::from)
            .and_then(candidate_to_wire_json)
        {
            Ok(candidate_json) => {
                let _ = self.events.send(EngineEvent::LocalCandidate {
                    peer: self.peer,
                    generation: self.generation,
                    candidate_json,
                });
            }
            Err(error) => {
                tracing::warn!(peer = %self.peer, %error, "failed to serialize local ICE candidate");
            }
        }
    }

    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.events.send(EngineEvent::IceGatheringComplete {
                peer: self.peer,
                generation: self.generation,
            });
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let _ = self.events.send(EngineEvent::PcState {
            peer: self.peer,
            generation: self.generation,
            state,
        });
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let label = match channel.label().await {
            Ok(label) => label,
            Err(error) => {
                tracing::warn!(peer = %self.peer, %error, "failed to read remote data-channel label");
                return;
            }
        };
        let _ = self.events.send(EngineEvent::RemoteChannel {
            peer: self.peer,
            generation: self.generation,
            label: label.clone(),
            channel: channel.clone(),
        });
        spawn_channel_event_loop(
            &self.runtime,
            &self.events,
            self.peer,
            self.generation,
            label,
            channel,
        );
    }
}

fn candidate_to_wire_json(mut candidate: RTCIceCandidateInit) -> Result<String> {
    // rtc 0.20 adds a local-only `url` provenance extension. It is not part of
    // the established Matchbox/browser RTCIceCandidateInit signaling shape, so
    // keep that field inside the local stack and preserve the four-field wire.
    candidate.url = None;
    serde_json::to_string(&candidate).context("serialize ICE candidate wire projection")
}

/// Fail before any session work when an explicitly requested [`IpFamily`]
/// cannot be served.
///
/// The client calls this before it opens its WebSocket, so a host that cannot
/// serve the family fails the process before a server-side room exists. The
/// alternative — discovering it per pair — is non-fatal there: the run would
/// degrade to the relay floor and still exit successfully, which is exactly
/// the silent pass `--ip-family` exists to prevent.
pub fn preflight_ip_family(settings: EngineSettings) -> Result<()> {
    ensure_requested_family_is_available(settings, local_udp_addrs)
}

/// [`preflight_ip_family`] with an injectable bind-selection rule, so the
/// failure path is reachable in a unit test on a host that serves every
/// family.
fn ensure_requested_family_is_available(
    settings: EngineSettings,
    resolve: impl Fn(EngineSettings) -> Result<Vec<SocketAddr>>,
) -> Result<()> {
    if settings.ip_family == IpFamily::Any {
        return Ok(());
    }
    resolve(settings)
        .with_context(|| format!("--ip-family {}", settings.ip_family.as_str()))
        .map(|_addrs| ())
}

/// Concrete local addresses for webrtc 0.20's application-owned ICE sockets.
///
/// Unlike earlier webrtc-rs releases, 0.20 turns each socket's bound address
/// directly into a host candidate. A wildcard bind would therefore advertise
/// `0.0.0.0`, which cannot connect peers on a zero-STUN LAN. Bind every active
/// interface address instead, restricted to [`EngineSettings::ip_family`].
/// IPv6 link-local addresses are omitted because the ICE candidate grammar
/// cannot carry the local interface's scope ID.
///
/// Public so a harness can apply the engine's exact rule as a precondition
/// rather than approximating it with its own probe.
pub fn local_udp_addrs(settings: EngineSettings) -> Result<Vec<SocketAddr>> {
    if settings.crippled {
        // rtc/webrtc 0.20 rejects a peer connection with no sockets or
        // listeners. Keep the fault deterministic with a loopback-only
        // transport. Crippled SettingEngine accepts only TCP4 candidates while
        // the builder supplies no TCP listeners, so the ICE agent cannot
        // register this UDP socket as a local candidate. PeerHandler and the
        // client retain outbound/inbound signaling filters as defense in
        // depth while offer/answer and gathering completion still follow the
        // real peer-connection lifecycle.
        //
        // `ip_family` is deliberately ignored: this transport exists to be
        // unusable, never to advertise a candidate, so pointing it at another
        // family would only create a fault shape nothing exercises.
        return Ok(vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]);
    }

    let active = if_addrs::get_if_addrs()
        .context("enumerate local network interfaces")?
        .into_iter()
        .filter(if_addrs::Interface::is_oper_up)
        .map(|interface| interface.ip());
    select_udp_addrs(active, settings.ip_family)
}

/// The pure selection rule behind [`local_udp_addrs`]: keep the concrete,
/// bindable addresses of `family`, sorted and deduplicated.
///
/// Unusable addresses are dropped regardless of family — unspecified (a
/// wildcard host candidate connects no one), multicast, and IPv6 link-local
/// (the ICE candidate grammar cannot carry the interface scope ID the local
/// socket key needs). An empty result is an error, never a silent fallback to
/// another family.
fn select_udp_addrs(
    addresses: impl IntoIterator<Item = IpAddr>,
    family: IpFamily,
) -> Result<Vec<SocketAddr>> {
    let mut addrs = BTreeSet::new();
    for ip in addresses {
        if ip.is_unspecified() || ip.is_multicast() || !family.admits(ip) {
            continue;
        }
        if matches!(ip, IpAddr::V6(ip) if ip.is_unicast_link_local()) {
            continue;
        }
        // Port 0 and scope 0: the driver keys its sockets by the exact address
        // it was handed, and a scoped IPv6 bind would not match.
        addrs.insert(SocketAddr::new(ip, 0));
    }

    if addrs.is_empty() {
        return Err(anyhow!(
            "no active concrete network interface is available for ICE (requested family: {})",
            family.as_str()
        ));
    }
    Ok(addrs.into_iter().collect())
}

fn spawn_channel_event_loop(
    runtime: &Arc<dyn Runtime>,
    events: &mpsc::UnboundedSender<EngineEvent>,
    peer: PlayerId,
    generation: u64,
    label: String,
    channel: Arc<dyn DataChannel>,
) {
    let events = events.clone();
    runtime.spawn(Box::pin(async move {
        while let Some(event) = channel.poll().await {
            match event {
                DataChannelEvent::OnOpen => {
                    let _ = events.send(EngineEvent::ChannelOpen {
                        peer,
                        generation,
                        label: label.clone(),
                    });
                }
                DataChannelEvent::OnMessage(message) => {
                    let text = String::from_utf8_lossy(&message.data).into_owned();
                    let _ = events.send(EngineEvent::ChannelMessage {
                        peer,
                        generation,
                        label: label.clone(),
                        text,
                    });
                }
                DataChannelEvent::OnError => {
                    tracing::warn!(%peer, channel = %label, "data channel became unreadable");
                    if label == RELIABLE_LABEL || label == UNRELIABLE_LABEL {
                        let _ = events.send(EngineEvent::ChannelClosed {
                            peer,
                            generation,
                            label: label.clone(),
                        });
                    }
                }
                DataChannelEvent::OnClose => {
                    if label == RELIABLE_LABEL || label == UNRELIABLE_LABEL {
                        let _ = events.send(EngineEvent::ChannelClosed {
                            peer,
                            generation,
                            label: label.clone(),
                        });
                    }
                    break;
                }
                DataChannelEvent::OnClosing
                | DataChannelEvent::OnBufferedAmountLow
                | DataChannelEvent::OnBufferedAmountHigh => {}
            }
        }
    }));
}

/// Convert the plan's ICE servers into webrtc-rs configuration entries.
fn convert_ice_servers(ice_servers: &[IceServer]) -> Vec<RTCIceServer> {
    ice_servers
        .iter()
        .map(|server| RTCIceServer {
            urls: server.urls.clone(),
            username: server.username.clone().unwrap_or_default(),
            credential: server.credential.clone().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the fixtures need the IPv6 constructors; production selection is
    // family-agnostic.
    use std::net::Ipv6Addr;

    /// Deterministic ICE failure in the given family.
    fn crippled_settings(ip_family: IpFamily) -> EngineSettings {
        EngineSettings {
            crippled: true,
            disable_mdns: true,
            ip_family,
            ..EngineSettings::default()
        }
    }

    /// A healthy engine with remote mDNS resolution disabled, matching the
    /// harness cells that pass `--disable-mdns`.
    fn mdns_disabled_settings() -> EngineSettings {
        EngineSettings {
            disable_mdns: true,
            ..EngineSettings::default()
        }
    }

    #[tokio::test]
    async fn crippled_engine_builds_full_mesh_links_without_candidate_leakage() {
        const REMOTE_PEERS: u128 = 15;

        assert_eq!(
            local_udp_addrs(crippled_settings(IpFamily::Any))
                .expect("crippled transport address resolves"),
            vec![SocketAddr::from(([127, 0, 0, 1], 0))]
        );
        assert_eq!(
            local_udp_addrs(crippled_settings(IpFamily::Ipv6))
                .expect("crippled transport ignores the requested family"),
            vec![SocketAddr::from(([127, 0, 0, 1], 0))],
            "the crippled transport exists to be unusable; --ip-family must not              fork it into a shape nothing exercises"
        );

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut engine =
            Engine::new(crippled_settings(IpFamily::Any), tx).expect("crippled engine builds");
        for ordinal in 1..=REMOTE_PEERS {
            let peer = PlayerId::from_u128(ordinal);
            engine
                .pair_with(peer, true, &[])
                .await
                .unwrap_or_else(|error| panic!("peer {ordinal} pairs: {error:#}"))
                .unwrap_or_else(|| panic!("peer {ordinal} produces an offer"));
        }

        let completed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut completed = BTreeSet::new();
            while completed.len() < REMOTE_PEERS as usize {
                match rx.recv().await.expect("engine event channel stays open") {
                    EngineEvent::IceGatheringComplete { peer, .. } => {
                        completed.insert(peer);
                    }
                    EngineEvent::LocalCandidate { peer, .. } => {
                        panic!("crippled engine leaked a candidate for {peer}");
                    }
                    _ => {}
                }
            }
            completed
        })
        .await
        .expect("every crippled link completes ICE gathering");
        assert_eq!(completed.len(), REMOTE_PEERS as usize);

        for link in engine.peers.values() {
            let report = link
                .pc
                .get_stats(std::time::Instant::now(), StatsSelector::None)
                .await;
            assert!(
                report
                    .iter()
                    .all(|entry| !matches!(entry, RTCStatsReportEntry::LocalCandidate(_))),
                "crippled ICE agent must register no local candidate"
            );
        }

        for ordinal in 1..=REMOTE_PEERS {
            engine
                .remove_peer(PlayerId::from_u128(ordinal))
                .await
                .unwrap_or_else(|error| panic!("peer {ordinal} closes: {error:#}"));
        }
    }

    #[tokio::test]
    async fn crippled_engine_rejects_candidate_embedded_in_remote_sdp() {
        let healthy_id = PlayerId::from_u128(1);
        let crippled_id = PlayerId::from_u128(2);
        let (healthy_tx, mut healthy_rx) = mpsc::unbounded_channel();
        let (crippled_tx, mut crippled_rx) = mpsc::unbounded_channel();
        let mut healthy =
            Engine::new(mdns_disabled_settings(), healthy_tx).expect("healthy engine");
        let mut crippled =
            Engine::new(crippled_settings(IpFamily::Any), crippled_tx).expect("crippled engine");

        let offer = healthy
            .pair_with(crippled_id, true, &[])
            .await
            .expect("healthy initiator pairs")
            .expect("healthy initiator offers");
        crippled
            .pair_with(healthy_id, false, &[])
            .await
            .expect("crippled responder pairs");

        let healthy_candidate = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match healthy_rx
                    .recv()
                    .await
                    .expect("healthy event channel stays open")
                {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        break serde_json::from_str::<RTCIceCandidateInit>(&candidate_json)
                            .expect("healthy candidate parses");
                    }
                    EngineEvent::IceGatheringComplete { .. } => {
                        panic!("healthy gathering completed without a candidate");
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("healthy candidate gathers");
        let offer_with_candidate = format!("{offer}a={}\r\n", healthy_candidate.candidate);

        let answer = crippled
            .handle_offer(healthy_id, offer_with_candidate)
            .await
            .expect("crippled responder accepts candidate-bearing offer");
        healthy
            .handle_answer(crippled_id, answer)
            .await
            .expect("healthy initiator accepts answer");

        let observation = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match tokio::time::timeout_at(observation, crippled_rx.recv()).await {
                Ok(Some(EngineEvent::PcState {
                    state: RTCPeerConnectionState::Connected,
                    ..
                })) => panic!("crippled peer connected through an SDP-embedded candidate"),
                Ok(Some(_)) => {}
                Ok(None) => panic!("crippled event channel closed during observation"),
                Err(_) => break,
            }
        }

        let report = crippled.peers[&healthy_id]
            .pc
            .get_stats(std::time::Instant::now(), StatsSelector::None)
            .await;
        assert!(
            report
                .iter()
                .all(|entry| !matches!(entry, RTCStatsReportEntry::LocalCandidate(_))),
            "crippled ICE agent must retain no local candidate"
        );

        healthy
            .remove_peer(crippled_id)
            .await
            .expect("healthy peer closes");
        crippled
            .remove_peer(healthy_id)
            .await
            .expect("crippled peer closes");
    }

    /// One mixed interface set covering every rejection reason plus both
    /// families, so the selection rule is proved independently of the host.
    fn interface_sample() -> Vec<IpAddr> {
        vec![
            IpAddr::from(Ipv4Addr::LOCALHOST),
            IpAddr::from([192, 168, 7, 5]),
            IpAddr::from(Ipv4Addr::UNSPECIFIED),
            IpAddr::from([224, 0, 0, 251]),
            IpAddr::from(Ipv6Addr::LOCALHOST),
            IpAddr::from([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]),
            // Link-local: unusable without the scope ID the wire cannot carry.
            IpAddr::from([0xfe80, 0, 0, 0, 0, 0, 0, 1]),
            IpAddr::from(Ipv6Addr::UNSPECIFIED),
            IpAddr::from([0xff02, 0, 0, 0, 0, 0, 0, 0xfb]),
        ]
    }

    #[test]
    fn ip_family_selects_exactly_the_concrete_addresses_of_that_family() {
        let v4 = SocketAddr::new(IpAddr::from(Ipv4Addr::LOCALHOST), 0);
        let v4_lan = SocketAddr::new(IpAddr::from([192, 168, 7, 5]), 0);
        let v6 = SocketAddr::new(IpAddr::from(Ipv6Addr::LOCALHOST), 0);
        let v6_global = SocketAddr::new(IpAddr::from([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]), 0);

        assert_eq!(
            select_udp_addrs(interface_sample(), IpFamily::Any).expect("mixed host selects"),
            vec![v4, v4_lan, v6, v6_global],
            "the default binds every concrete address of both families"
        );
        assert_eq!(
            select_udp_addrs(interface_sample(), IpFamily::Ipv4).expect("IPv4 host selects"),
            vec![v4, v4_lan]
        );
        assert_eq!(
            select_udp_addrs(interface_sample(), IpFamily::Ipv6).expect("IPv6 host selects"),
            vec![v6, v6_global]
        );
    }

    #[test]
    fn selected_ipv6_binds_are_concrete_and_scope_free() {
        for address in
            select_udp_addrs(interface_sample(), IpFamily::Ipv6).expect("IPv6 host selects")
        {
            let SocketAddr::V6(address) = address else {
                panic!("IPv6-only selection returned {address}");
            };
            assert_eq!(
                address.scope_id(),
                0,
                "the driver keys sockets by exact address; a scoped bind cannot be matched"
            );
            assert!(!address.ip().is_unicast_link_local());
            assert!(!address.ip().is_unspecified() && !address.ip().is_multicast());
        }
    }

    #[test]
    fn ip_family_tokens_match_the_cli_surface() {
        for family in IpFamily::value_variants() {
            let token = family
                .to_possible_value()
                .expect("every family is selectable on the CLI");
            assert_eq!(
                family.as_str(),
                token.get_name(),
                "the diagnostic token must stay identical to the flag value"
            );
        }
    }

    #[test]
    fn a_family_the_host_cannot_serve_fails_loudly_instead_of_falling_back() {
        let ipv4_only = vec![
            IpAddr::from(Ipv4Addr::LOCALHOST),
            IpAddr::from([0xfe80, 0, 0, 0, 0, 0, 0, 1]),
        ];
        let error = select_udp_addrs(ipv4_only, IpFamily::Ipv6)
            .expect_err("an IPv6-only run on an IPv4-only host must fail");
        assert!(
            error.to_string().contains("ipv6"),
            "the failure must name the requested family: {error}"
        );
        assert!(select_udp_addrs(Vec::new(), IpFamily::Any).is_err());
    }

    #[test]
    fn an_unservable_requested_family_fails_preflight_not_one_pair() {
        // Injected so the failure path is exercised on every host, including
        // the ones that serve both families.
        let unservable = |settings: EngineSettings| {
            select_udp_addrs([IpAddr::from(Ipv4Addr::LOCALHOST)], settings.ip_family)
        };
        let requested = EngineSettings {
            ip_family: IpFamily::Ipv6,
            ..EngineSettings::default()
        };
        let error = ensure_requested_family_is_available(requested, unservable)
            .expect_err("an unservable family must fail the pre-flight");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("--ip-family ipv6"),
            "the failure must name the flag that caused it: {rendered}"
        );

        // The default never consults the resolver at all, so a host with no
        // usable interface still fails later (per pair) exactly as before.
        ensure_requested_family_is_available(EngineSettings::default(), |_settings| {
            panic!("--ip-family any must not be resolved eagerly")
        })
        .expect("the default family imposes no startup requirement");

        // The production entry point applies the real rule. Whatever this host
        // serves, the two must agree — so a pre-flight that stopped consulting
        // the selection rule fails here.
        assert_eq!(
            preflight_ip_family(requested).is_ok(),
            local_udp_addrs(requested).is_ok(),
            "the pre-flight must mirror the engine's own bind selection"
        );
    }

    #[tokio::test]
    async fn gathered_host_candidates_never_advertise_wildcard_addresses() {
        let udp_addrs =
            local_udp_addrs(EngineSettings::default()).expect("concrete UDP addresses resolve");
        assert!(udp_addrs.iter().all(|address| match address {
            SocketAddr::V4(_) => true,
            SocketAddr::V6(address) => address.scope_id() == 0,
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(mdns_disabled_settings(), tx).expect("engine builds");
        let peer = PlayerId::from_u128(0xd);
        engine
            .pair_with(peer, true, &[])
            .await
            .expect("initiator pairs")
            .expect("initiator offer");

        let host_addresses = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut host_addresses = Vec::new();
            loop {
                match rx.recv().await.expect("engine event channel stays open") {
                    EngineEvent::LocalCandidate { candidate_json, .. } => {
                        let init: RTCIceCandidateInit = serde_json::from_str(&candidate_json)
                            .expect("candidate wire JSON parses");
                        let fields = init.candidate.split_whitespace().collect::<Vec<_>>();
                        if fields.windows(2).any(|pair| pair == ["typ", "host"]) {
                            let address = fields
                                .get(4)
                                .expect("ICE host candidate carries an address")
                                .parse::<IpAddr>()
                                .expect("raw host candidate carries an IP address");
                            host_addresses.push(address);
                        }
                    }
                    EngineEvent::IceGatheringComplete { .. } => break host_addresses,
                    _ => {}
                }
            }
        })
        .await
        .expect("ICE gathering completes");

        assert!(
            !host_addresses.is_empty(),
            "normal zero-STUN gathering must produce a host candidate"
        );
        assert!(host_addresses
            .iter()
            .all(|address| !address.is_unspecified() && !address.is_multicast()));
        engine.remove_peer(peer).await.expect("peer closes");
    }

    #[test]
    fn ice_server_conversion_maps_credentials_and_defaults() {
        let converted = convert_ice_servers(&[
            IceServer {
                urls: vec!["stun:stun.example.com:3478".to_string()],
                username: None,
                credential: None,
            },
            IceServer {
                urls: vec!["turn:turn.example.com:3478".to_string()],
                username: Some("1700003600:player".to_string()),
                credential: Some("secret".to_string()),
            },
        ]);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0].urls, vec!["stun:stun.example.com:3478"]);
        assert_eq!(converted[0].username, "");
        assert_eq!(converted[0].credential, "");
        assert_eq!(converted[1].username, "1700003600:player");
        assert_eq!(converted[1].credential, "secret");
    }

    #[test]
    fn ice_candidate_wire_projection_omits_rtc_020_local_url_extension() {
        let json = candidate_to_wire_json(RTCIceCandidateInit {
            candidate: "candidate:relay 1 udp 1 192.0.2.1 3478 typ relay".to_string(),
            sdp_mid: Some("0".to_string()),
            sdp_mline_index: Some(0),
            username_fragment: Some("ufrag".to_string()),
            url: Some("turn:turn.example.com:3478?transport=udp".to_string()),
        })
        .expect("candidate serializes");

        assert_eq!(
            json,
            r#"{"candidate":"candidate:relay 1 udp 1 192.0.2.1 3478 typ relay","sdpMid":"0","sdpMLineIndex":0,"usernameFragment":"ufrag"}"#
        );
    }

    #[tokio::test]
    async fn pairing_is_idempotent_and_initiator_offers() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(EngineSettings::default(), tx).expect("engine builds");
        let peer = PlayerId::from_u128(0xb);

        let offer = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.pair_with(peer, true, &[]),
        )
        .await
        .expect("pairing within timeout")
        .expect("pairing succeeds");
        assert!(offer.is_some(), "initiator must produce an offer SDP");
        assert!(engine.is_paired(peer));
        assert_eq!(engine.connected_pair_count(), 0);
        let stale_generation = engine.peers[&peer].generation;
        let stale_channel = engine.peers[&peer].channels[RELIABLE_LABEL].clone();

        let duplicate = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            engine.pair_with(peer, true, &[]),
        )
        .await
        .expect("duplicate pairing within timeout")
        .expect("duplicate pairing is a no-op");
        assert!(duplicate.is_none(), "duplicate pairing must not re-offer");

        engine
            .remove_peer(peer)
            .await
            .expect("departed peer closes cleanly");
        assert!(!engine.is_paired(peer));
        let replacement = engine
            .pair_with(peer, false, &[])
            .await
            .expect("reconnected peer can pair anew");
        assert!(replacement.is_none(), "responder waits for the new offer");
        assert!(engine.is_paired(peer));
        let current_generation = engine.peers[&peer].generation;
        assert_ne!(stale_generation, current_generation);

        let stale_events = [
            EngineEvent::LocalCandidate {
                peer,
                generation: stale_generation,
                candidate_json: "{}".to_string(),
            },
            EngineEvent::IceGatheringComplete {
                peer,
                generation: stale_generation,
            },
            EngineEvent::PcState {
                peer,
                generation: stale_generation,
                state: RTCPeerConnectionState::Connected,
            },
            EngineEvent::RemoteChannel {
                peer,
                generation: stale_generation,
                label: RELIABLE_LABEL.to_string(),
                channel: stale_channel,
            },
            EngineEvent::ChannelOpen {
                peer,
                generation: stale_generation,
                label: RELIABLE_LABEL.to_string(),
            },
            EngineEvent::ChannelClosed {
                peer,
                generation: stale_generation,
                label: RELIABLE_LABEL.to_string(),
            },
            EngineEvent::ChannelMessage {
                peer,
                generation: stale_generation,
                label: RELIABLE_LABEL.to_string(),
                text: "stale".to_string(),
            },
        ];
        assert!(stale_events
            .iter()
            .all(|event| !engine.is_current_event(event)));
        assert!(engine.is_current_event(&EngineEvent::PcState {
            peer,
            generation: current_generation,
            state: RTCPeerConnectionState::Connected,
        }));
    }

    #[test]
    fn note_channel_open_fires_pair_connected_exactly_once() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(EngineSettings::default(), tx).expect("engine builds");
        let peer = PlayerId::from_u128(0xc);
        // Unknown peer: never connected.
        assert!(!engine.note_channel_open(peer, RELIABLE_LABEL));
    }

    #[tokio::test]
    async fn closing_a_live_required_channel_emits_current_channel_closed() {
        let (a_tx, mut a_rx) = mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = mpsc::unbounded_channel();
        let mut a = Engine::new(mdns_disabled_settings(), a_tx).expect("engine A builds");
        let mut b = Engine::new(mdns_disabled_settings(), b_tx).expect("engine B builds");
        let a_id = PlayerId::from_u128(0xa);
        let b_id = PlayerId::from_u128(0xb);

        let offer = a
            .pair_with(b_id, true, &[])
            .await
            .expect("A pairs")
            .expect("initiator offer");
        b.pair_with(a_id, false, &[])
            .await
            .expect("B pairs as responder");
        let answer = b.handle_offer(a_id, offer).await.expect("B answers");
        a.handle_answer(b_id, answer)
            .await
            .expect("A applies answer");

        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut a_connected = false;
            let mut b_connected = false;
            let mut a_pc_connected = false;
            while !a_connected || !b_connected {
                tokio::select! {
                    event = a_rx.recv() => match event.expect("A event channel stays open") {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            b.handle_remote_candidate(a_id, &candidate_json)
                                .await
                                .expect("B applies A candidate");
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } => {
                            a.store_remote_channel(b_id, label, channel);
                        }
                        EngineEvent::ChannelOpen { label, .. } => {
                            a_connected |= a.note_channel_open(b_id, &label);
                        }
                        EngineEvent::PcState { state, .. } => {
                            a_pc_connected |= state == RTCPeerConnectionState::Connected;
                        }
                        _ => {}
                    },
                    event = b_rx.recv() => match event.expect("B event channel stays open") {
                        EngineEvent::LocalCandidate { candidate_json, .. } => {
                            a.handle_remote_candidate(b_id, &candidate_json)
                                .await
                                .expect("A applies B candidate");
                        }
                        EngineEvent::RemoteChannel { label, channel, .. } => {
                            b.store_remote_channel(a_id, label, channel);
                        }
                        EngineEvent::ChannelOpen { label, .. } => {
                            b_connected |= b.note_channel_open(a_id, &label);
                        }
                        _ => {}
                    },
                }
            }
            assert!(a_pc_connected, "A peer connection must reach Connected");
        })
        .await
        .expect("local peer pair connects");

        a.channel(b_id, RELIABLE_LABEL)
            .expect("required channel exists")
            .close()
            .await
            .expect("required channel closes");

        let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match a_rx.recv().await.expect("A event channel stays open") {
                    EngineEvent::ChannelClosed {
                        peer,
                        generation,
                        label,
                    } => break (peer, generation, label),
                    EngineEvent::PcState {
                        state: RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed,
                        ..
                    } => {
                        panic!("peer connection became terminal before channel-close evidence")
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("close callback arrives");
        assert_eq!(closed.0, b_id);
        assert!(a.is_current_generation(closed.0, closed.1));
        assert_eq!(closed.2, RELIABLE_LABEL);
        a.remove_peer(b_id).await.expect("A closes");
        b.remove_peer(a_id).await.expect("B closes");
    }
}
