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
//! - **Crippled mode** (`--cripple-ice`): the peer connection binds no UDP
//!   transport sockets, and candidate signals are
//!   dropped in both directions — deterministic non-connectivity for fallback
//!   scenarios.
//!
//! The engine is owned and driven by the single orchestrator task. webrtc-rs's
//! async peer handler and polled data channels never touch engine state directly;
//! they forward through an unbounded [`EngineEvent`] channel back to the orchestrator.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use rtc::ice::mdns::MulticastDnsMode;
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

/// The per-client WebRTC engine. Owned by the orchestrator task; all methods
/// are called from that single task, so no interior locking is needed.
pub struct Engine {
    crippled: bool,
    disable_mdns: bool,
    runtime: Arc<dyn Runtime>,
    events: mpsc::UnboundedSender<EngineEvent>,
    peers: HashMap<PlayerId, PeerLink>,
    next_generation: u64,
    relay_only: bool,
}

impl Engine {
    /// Build the engine with the default runtime and settings unless a harness
    /// requests deterministic ICE failure or disables remote mDNS resolution.
    pub fn new(
        crippled: bool,
        disable_mdns: bool,
        relay_only: bool,
        events: mpsc::UnboundedSender<EngineEvent>,
    ) -> Result<Self> {
        let runtime = default_runtime()
            .ok_or_else(|| anyhow!("webrtc 0.20 was built without an async runtime feature"))?;
        Ok(Self {
            crippled,
            disable_mdns,
            runtime,
            events,
            peers: HashMap::new(),
            next_generation: 0,
            relay_only,
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
            .with_ice_transport_policy(if self.relay_only {
                RTCIceTransportPolicy::Relay
            } else {
                RTCIceTransportPolicy::All
            })
            .build();
        let mut setting_engine = SettingEngine::default();
        if self.disable_mdns {
            setting_engine.set_multicast_dns_mode(MulticastDnsMode::Disabled);
        }
        let handler = Arc::new(PeerHandler {
            peer,
            generation,
            crippled: self.crippled,
            events: self.events.clone(),
            runtime: self.runtime.clone(),
        });
        let udp_addrs = local_udp_addrs(self.crippled)?;
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

    /// Candidate types of the selected ICE path for a connected peer.
    pub async fn selected_candidate_types(&self, peer: PlayerId) -> Option<(String, String)> {
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
        Some((
            local.candidate_type.to_string(),
            remote.candidate_type.to_string(),
        ))
    }
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

/// Concrete local addresses for webrtc 0.20's application-owned ICE sockets.
///
/// Unlike earlier webrtc-rs releases, 0.20 turns each socket's bound address
/// directly into a host candidate. A wildcard bind would therefore advertise
/// `0.0.0.0`, which cannot connect peers on a zero-STUN LAN. Bind every active
/// interface address instead. IPv6 link-local addresses are omitted because
/// the ICE candidate grammar cannot carry the local interface's scope ID.
fn local_udp_addrs(crippled: bool) -> Result<Vec<SocketAddr>> {
    if crippled {
        return Ok(Vec::new());
    }

    let mut addrs = BTreeSet::new();
    for interface in if_addrs::get_if_addrs().context("enumerate local network interfaces")? {
        if !interface.is_oper_up() {
            continue;
        }
        let ip = interface.ip();
        if ip.is_unspecified() || ip.is_multicast() {
            continue;
        }
        let addr = match ip {
            IpAddr::V6(ip) if ip.is_unicast_link_local() => continue,
            IpAddr::V6(ip) => SocketAddr::new(IpAddr::V6(ip), 0),
            IpAddr::V4(ip) => SocketAddr::new(IpAddr::V4(ip), 0),
        };
        addrs.insert(addr);
    }

    if addrs.is_empty() {
        return Err(anyhow!(
            "no active concrete network interface is available for ICE"
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

    #[tokio::test]
    async fn gathered_host_candidates_never_advertise_wildcard_addresses() {
        let udp_addrs = local_udp_addrs(false).expect("concrete UDP addresses resolve");
        assert!(udp_addrs.iter().all(|address| match address {
            SocketAddr::V4(_) => true,
            SocketAddr::V6(address) => address.scope_id() == 0,
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(false, true, false, tx).expect("engine builds");
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
        let mut engine = Engine::new(false, false, false, tx).expect("engine builds");
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
        let mut engine = Engine::new(false, false, false, tx).expect("engine builds");
        let peer = PlayerId::from_u128(0xc);
        // Unknown peer: never connected.
        assert!(!engine.note_channel_open(peer, RELIABLE_LABEL));
    }

    #[tokio::test]
    async fn closing_a_live_required_channel_emits_current_channel_closed() {
        let (a_tx, mut a_rx) = mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = mpsc::unbounded_channel();
        let mut a = Engine::new(false, true, false, a_tx).expect("engine A builds");
        let mut b = Engine::new(false, true, false, b_tx).expect("engine B builds");
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
