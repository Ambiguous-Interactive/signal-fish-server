//! WebRTC engine: one `RTCPeerConnection` per remote peer, two data channels
//! per pair, trickle ICE relayed through the server's opaque `Signal` envelope.
//!
//! # Protocol steps implemented here (`docs/protocol.md`, ADR-0002)
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
//! they forward through an [`EngineEvent`] channel back to the orchestrator.
//! Message permits bound pending payloads without blocking lifecycle notifications.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use crate::deadline::Deadline;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use clap::ValueEnum;
use rtc::ice::{mdns::MulticastDnsMode, network_type::NetworkType, url::SchemeType, url::Url};
use rtc::peer_connection::transport::RTCIceCandidatePair;
use signal_fish_server::protocol::{IceServer, PlayerId};
use tokio::sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceGatheringState, RTCIceServer, RTCIceTransportPolicy,
    RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription, SettingEngineBuilder,
};
#[cfg(test)]
use webrtc::peer_connection::{RTCStatsReportEntry, StatsSelector};
use webrtc::runtime::{default_runtime, JoinHandle, Runtime};

/// Label of the ordered, reliable data channel (commands / critical events).
pub const RELIABLE_LABEL: &str = "reliable";
/// Label of the unordered, no-retransmit channel (movement / state).
pub const UNRELIABLE_LABEL: &str = "unreliable";

// Per physical link, including candidates already passed to the WebRTC stack.
// Keep aligned with the browser engine; draining the queue must not renew it.
const MAX_REMOTE_CANDIDATES: usize = 128;
const MAX_REMOTE_CANDIDATE_BYTES: usize = 64 * 1024;
const MAX_CANDIDATE_PAYLOAD_BYTES: usize = 4096;

const MAX_CHANNEL_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_PENDING_CHANNEL_MESSAGES: usize = 256;
const MAX_PENDING_CHANNEL_BYTES: usize = 1024 * 1024;

struct MessageQueueBudget {
    messages: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

impl Default for MessageQueueBudget {
    fn default() -> Self {
        Self {
            messages: Arc::new(Semaphore::new(MAX_PENDING_CHANNEL_MESSAGES)),
            bytes: Arc::new(Semaphore::new(MAX_PENDING_CHANNEL_BYTES)),
        }
    }
}

impl MessageQueueBudget {
    fn reserve(&self, bytes: usize) -> Option<MessageQueuePermit> {
        let message = self.messages.clone().try_acquire_owned().ok()?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(u32::try_from(bytes).ok()?)
            .ok()?;
        Some(MessageQueuePermit {
            _message: message,
            _bytes: bytes,
        })
    }
}

/// Holds receive capacity until a message is handled or discarded.
pub struct MessageQueuePermit {
    _message: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

#[cfg(test)]
impl MessageQueuePermit {
    pub(crate) fn for_test() -> Self {
        MessageQueueBudget::default().reserve(0).unwrap()
    }
}

struct ChannelReceiveAdmission {
    #[cfg(test)]
    message_observed: std::sync::Mutex<Option<Arc<tokio::sync::Notify>>>,
    budget: Arc<MessageQueueBudget>,
    channels: Arc<Mutex<RequiredChannelAdmission>>,
    closed_labels: AtomicU8,
    rejected: Mutex<bool>,
    terminal: AtomicBool,
}

impl ChannelReceiveAdmission {
    async fn allows_work(
        &self,
        events: &mpsc::UnboundedSender<EngineEvent>,
        peer: PlayerId,
        generation: u64,
        channel_event_label: Option<&str>,
    ) -> bool {
        if self.terminal.load(Ordering::Relaxed) {
            return false;
        }
        let selected = {
            let channels = self.channels.lock().await;
            [
                (
                    RELIABLE_LABEL,
                    channels.reliable.as_ref().and_then(Weak::upgrade),
                ),
                (
                    UNRELIABLE_LABEL,
                    channels.unreliable.as_ref().and_then(Weak::upgrade),
                ),
            ]
        };
        for (label, channel) in selected {
            /* A channel's queued input precedes its own close event. */
            if channel_event_label == Some(label) {
                continue;
            }
            let Some(channel) = channel else { continue };
            if matches!(
                channel.ready_state().await,
                Ok(rtc::data_channel::RTCDataChannelState::Closing
                    | rtc::data_channel::RTCDataChannelState::Closed)
                    | Err(_)
            ) {
                if !self.terminal.swap(true, Ordering::Relaxed) {
                    self.report_channel_closed(events, peer, generation, label);
                }
                return false;
            }
        }
        !self.terminal.load(Ordering::Relaxed)
    }

    fn report_channel_closed(
        &self,
        events: &mpsc::UnboundedSender<EngineEvent>,
        peer: PlayerId,
        generation: u64,
        label: &str,
    ) {
        let bit = match label {
            RELIABLE_LABEL => 1,
            UNRELIABLE_LABEL => 2,
            _ => return,
        };
        if self.closed_labels.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
            let _ = events.send(EngineEvent::ChannelClosed {
                peer,
                generation,
                label: label.into(),
            });
        }
    }

    fn decode_and_reserve(&self, data: &[u8]) -> Option<(String, MessageQueuePermit)> {
        if data.len() > MAX_CHANNEL_MESSAGE_BYTES {
            return None;
        }
        let text = String::from_utf8_lossy(data);
        if text.len() > MAX_CHANNEL_MESSAGE_BYTES {
            return None;
        }
        let permit = self.budget.reserve(text.len())?;
        Some((text.into_owned(), permit))
    }
}

/// Notifications from webrtc-rs callbacks back to the orchestrator task.
/// (No `Debug` derive: `RTCDataChannel` is not `Debug`.)
pub enum EngineEvent {
    /// A local ICE candidate was gathered; `candidate_json` is the serialized
    /// `RTCIceCandidateInit` to relay as `{"IceCandidate": candidate_json}`,
    /// and `gathered` its typed projection for the JSONL event stream.
    LocalCandidate {
        peer: PlayerId,
        generation: u64,
        candidate_json: String,
        gathered: GatheredCandidate,
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
    /// A required data channel started closing, closed, or became unreadable.
    ChannelClosed {
        peer: PlayerId,
        generation: u64,
        label: String,
    },
    /// A text message arrived on an open data channel. Keep `queue_permit`
    /// until its text is consumed or discarded, including asynchronous output.
    /// `received_at` records the monotonic channel-poll completion time.
    ChannelMessage {
        peer: PlayerId,
        generation: u64,
        label: String,
        text: String,
        queue_permit: MessageQueuePermit,
        received_at: tokio::time::Instant,
    },
    /// Receive limits failed; the orchestrator must retire this physical link.
    ChannelReceiveRejected {
        peer: PlayerId,
        generation: u64,
        label: String,
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
            }
            | Self::ChannelReceiveRejected {
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
    /// Shared with the callback so unread announcements retain at most two channels.
    channel_admission: Arc<Mutex<RequiredChannelAdmission>>,
    receive_admission: Arc<ChannelReceiveAdmission>,
    /// Labels observed open so far; the pair is connected when both labels are in.
    open_labels: BTreeSet<String>,
    /// Remote ICE candidates cannot be applied before the remote description.
    pending_candidates: Vec<RTCIceCandidateInit>,
    remote_candidate_count: usize,
    remote_candidate_bytes: usize,
    remote_description_set: bool,
    pair_connected: bool,
    /// At most two admitted channel pollers, owned by this physical link.
    channel_tasks: Vec<Box<dyn JoinHandle>>,
}

impl PeerLink {
    fn stop_channel_tasks(&mut self) {
        for task in self.channel_tasks.drain(..) {
            task.abort();
        }
    }
}

impl Drop for PeerLink {
    fn drop(&mut self) {
        // Runtime handles detach on drop. Explicit cancellation also covers
        // failed pairing, cancelled teardown, and destruction of the engine.
        self.stop_channel_tasks();
    }
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

pub(crate) enum Negotiation {
    Pair {
        initiate: bool,
        ice_servers: Vec<IceServer>,
    },
    Offer(String),
    Answer(String),
    Candidate(String),
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct NegotiationHold {
    pub step: &'static str,
    pub entered: Arc<tokio::sync::Notify>,
    pub release: Arc<tokio::sync::Notify>,
}

macro_rules! negotiation_checkpoint {
    ($hold:ident, $step:literal) => {
        #[cfg(test)]
        if let Some(hold) = &$hold {
            if hold.step == $step {
                hold.entered.notify_one();
                hold.release.notified().await;
            }
        }
    };
}

/// The per-client WebRTC engine. Owned by the orchestrator task; all methods
/// are called from that single task, so no interior locking is needed.
pub struct Engine {
    #[cfg(test)]
    pub(crate) negotiation_hold: Option<NegotiationHold>,
    settings: EngineSettings,
    runtime: Arc<dyn Runtime>,
    events: mpsc::UnboundedSender<EngineEvent>,
    message_budget: Arc<MessageQueueBudget>,
    peers: HashMap<PlayerId, PeerLink>,
    next_generation: u64,
    receipt_generations: HashMap<PlayerId, (u64, u64)>,
    /// Routing answers for the current ICE endpoints, resolved once per set
    /// rather than once per pair.
    ///
    /// Pairing runs on the orchestrator task, which also pumps the WebSocket.
    /// Resolving every server for every peer would multiply any resolver
    /// latency by the mesh size on that task, and a long enough stall there is
    /// indistinguishable from an idle client to the server. The key is the
    /// endpoint list itself, so a replan that changes servers re-probes while a
    /// credential rotation — same URLs, new username and password — does not.
    ice_route_sources: Option<(Vec<(String, u16)>, Vec<IpAddr>)>,
}

impl Engine {
    /// Build the engine with the default runtime and the invocation's
    /// [`EngineSettings`].
    pub fn new(
        settings: EngineSettings,
        events: mpsc::UnboundedSender<EngineEvent>,
    ) -> Result<Self> {
        let runtime = default_runtime()
            .ok_or_else(|| anyhow!("webrtc 0.20 was built without an async runtime feature"))?;
        Ok(Self {
            #[cfg(test)]
            negotiation_hold: None,
            settings,
            runtime,
            events,
            message_budget: Arc::new(MessageQueueBudget::default()),
            peers: HashMap::new(),
            next_generation: 0,
            receipt_generations: HashMap::new(),
            ice_route_sources: None,
        })
    }

    /// Routing answers for `ice_servers`, resolved on first use and reused
    /// while the endpoint set is unchanged (see [`Engine::ice_route_sources`]).
    async fn ice_route_sources(&mut self, ice_servers: &[IceServer]) -> Vec<IpAddr> {
        let endpoints = ice_server_endpoints(ice_servers);
        if let Some((probed, sources)) = &self.ice_route_sources {
            if *probed == endpoints {
                return sources.clone();
            }
        }
        let sources = ice_server_source_addrs(&endpoints).await;
        self.ice_route_sources = Some((endpoints, sources.clone()));
        sources
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

    pub(crate) fn owns_pending_receipt(&self, peer: PlayerId, generation: u64) -> bool {
        self.receipt_generations
            .get(&peer)
            .is_some_and(|(first, last)| (*first..=*last).contains(&generation))
    }

    pub(crate) fn invalidate_pending_receipts(&mut self, peer: PlayerId) {
        self.receipt_generations.remove(&peer);
    }

    pub(crate) async fn negotiate(
        &mut self,
        peer: PlayerId,
        operation: Negotiation,
        deadline: Deadline,
    ) -> Result<Option<String>> {
        if deadline.is_due(tokio::time::Instant::now()) {
            self.report_peer_loss(peer, "negotiation deadline expired");
            self.remove_peer(peer).await?;
            return Err(anyhow!("WebRTC negotiation deadline elapsed"));
        }
        let operation = Box::pin(async {
            match operation {
                Negotiation::Pair {
                    initiate,
                    ice_servers,
                } => self.pair_with(peer, initiate, &ice_servers).await,
                Negotiation::Offer(sdp) => self.handle_offer(peer, sdp).await.map(Some),
                Negotiation::Answer(sdp) => self.handle_answer(peer, sdp).await.map(|()| None),
                Negotiation::Candidate(payload) => self
                    .handle_remote_candidate(peer, &payload)
                    .await
                    .map(|()| None),
            }
        });
        let result = deadline.timeout(operation).await;
        match result {
            Ok(result) if !deadline.is_due(tokio::time::Instant::now()) => result,
            _ => {
                self.report_peer_loss(peer, "negotiation deadline expired");
                self.remove_peer(peer).await?;
                Err(anyhow!("WebRTC negotiation deadline elapsed"))
            }
        }
    }

    /// Close and forget a departed peer so a later directive can pair it anew.
    pub async fn remove_peer(&mut self, peer: PlayerId) -> Result<()> {
        if let Some(mut link) = self.peers.remove(&peer) {
            link.stop_channel_tasks();
            #[cfg(test)]
            let negotiation_hold = self.negotiation_hold.clone();
            let result = crate::deadline::timeout(Duration::from_millis(250), async {
                negotiation_checkpoint!(negotiation_hold, "close");
                link.pc.close().await
            })
            .await;
            match result {
                Ok(result) => result.context("close departed RTCPeerConnection")?,
                Err(_) => tracing::warn!(%peer, "Peer close deadline elapsed; retry can continue"),
            }
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

        #[cfg(test)]
        let negotiation_hold = self.negotiation_hold.clone();
        negotiation_checkpoint!(negotiation_hold, "route");
        let config = RTCConfigurationBuilder::new()
            .with_ice_servers(convert_ice_servers(ice_servers))
            .with_ice_transport_policy(if self.settings.relay_only {
                RTCIceTransportPolicy::Relay
            } else {
                RTCIceTransportPolicy::All
            })
            .build();
        let mut setting_engine = SettingEngineBuilder::new();
        if self.settings.disable_mdns {
            setting_engine = setting_engine.with_multicast_dns_mode(MulticastDnsMode::Disabled);
        }
        if self.settings.crippled {
            // The driver requires at least one socket, but the ICE agent
            // must never register that dummy UDP socket as a usable local
            // candidate. Permit only TCP4 candidates while supplying no TCP
            // listeners. This also rejects UDP candidates embedded directly
            // in remote SDP, below the signaling layer's candidate filters.
            setting_engine = setting_engine.with_network_types(vec![NetworkType::Tcp4]);
        }
        let channel_admission = Arc::new(Mutex::new(RequiredChannelAdmission::default()));
        let receive_admission = Arc::new(ChannelReceiveAdmission {
            #[cfg(test)]
            message_observed: std::sync::Mutex::new(None),
            budget: self.message_budget.clone(),
            channels: channel_admission.clone(),
            closed_labels: AtomicU8::new(0),
            rejected: Mutex::new(false),
            terminal: AtomicBool::new(false),
        });
        let handler = Arc::new(PeerHandler {
            peer,
            generation,
            crippled: self.settings.crippled,
            events: self.events.clone(),
            channel_admission: channel_admission.clone(),
            receive_admission: receive_admission.clone(),
        });
        let udp_addrs = session_udp_addrs(
            self.settings,
            local_udp_addrs,
            self.ice_route_sources(ice_servers).await,
        )?;
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(config)
                .with_setting_engine(setting_engine.build())
                .with_handler(handler)
                .with_runtime(self.runtime.clone())
                .with_udp_addrs(udp_addrs)
                .build()
                .await
                .context("create peer connection")?,
        );
        negotiation_checkpoint!(negotiation_hold, "build");
        let link = PeerLink {
            generation,
            pc: pc.clone(),
            channels: HashMap::new(),
            channel_admission: channel_admission.clone(),
            open_labels: BTreeSet::new(),
            receive_admission: receive_admission.clone(),
            pending_candidates: Vec::new(),
            remote_candidate_count: 0,
            remote_candidate_bytes: 0,
            remote_description_set: false,
            pair_connected: false,
            channel_tasks: Vec::with_capacity(2),
        };
        self.peers.insert(peer, link);
        self.receipt_generations
            .entry(peer)
            .and_modify(|(_, last)| *last = generation)
            .or_insert((generation, generation));
        let result = async {
            if !initiate {
                return Ok(None);
            }
            // Both channels must exist before the offer so the SDP negotiates
            // SCTP and the channels open as soon as DTLS completes.
            negotiation_checkpoint!(negotiation_hold, "reliable");
            let reliable = pc
                .create_data_channel(RELIABLE_LABEL, None)
                .await
                .context("create reliable data channel")?;
            negotiation_checkpoint!(negotiation_hold, "unreliable");
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
            {
                let mut admission = channel_admission.lock().await;
                admission.select(RELIABLE_LABEL, &reliable);
                admission.select(UNRELIABLE_LABEL, &unreliable);
            }
            let reliable_task = spawn_channel_event_loop(
                &self.runtime,
                &self.events,
                peer,
                generation,
                RELIABLE_LABEL.to_string(),
                reliable.clone(),
                receive_admission.clone(),
            );
            let unreliable_task = spawn_channel_event_loop(
                &self.runtime,
                &self.events,
                peer,
                generation,
                UNRELIABLE_LABEL.to_string(),
                unreliable.clone(),
                receive_admission,
            );
            let link = self
                .peers
                .get_mut(&peer)
                .ok_or_else(|| anyhow!("peer link removed while pairing"))?;
            link.channel_tasks.extend([reliable_task, unreliable_task]);
            link.channels.insert(RELIABLE_LABEL.to_string(), reliable);
            link.channels
                .insert(UNRELIABLE_LABEL.to_string(), unreliable);

            negotiation_checkpoint!(negotiation_hold, "offer");
            let offer = pc.create_offer(None).await.context("create offer")?;
            let sdp = offer.sdp.clone();
            negotiation_checkpoint!(negotiation_hold, "offer_local");
            pc.set_local_description(offer)
                .await
                .context("set local description (offer)")?;
            Ok(Some(sdp))
        }
        .await;
        if result.is_err() {
            if let Some(mut link) = self.peers.remove(&peer) {
                link.stop_channel_tasks();
                let _ = link.pc.close().await;
            }
        }
        result
    }

    /// Responder path: apply a remote offer and produce the answer SDP (the
    /// caller relays it as `{"Answer": sdp}`). Buffered remote candidates are
    /// flushed once the remote description is set.
    pub async fn handle_offer(&mut self, peer: PlayerId, sdp: String) -> Result<String> {
        #[cfg(test)]
        let negotiation_hold = self.negotiation_hold.clone();
        let link = self
            .peers
            .get_mut(&peer)
            .ok_or_else(|| anyhow!("offer from unpaired peer {peer}"))?;
        let offer = RTCSessionDescription::offer(sdp).context("parse remote offer SDP")?;
        let pc = link.pc.clone();
        negotiation_checkpoint!(negotiation_hold, "remote_offer");
        pc.set_remote_description(offer)
            .await
            .context("set remote description (offer)")?;
        link.remote_description_set = true;
        let pending = std::mem::take(&mut link.pending_candidates);
        for candidate in pending {
            negotiation_checkpoint!(negotiation_hold, "buffered_ice");
            if pc.add_ice_candidate(candidate).await.is_err() {
                tracing::warn!(%peer, "WebRTC stack rejected a buffered ICE candidate");
            }
        }
        negotiation_checkpoint!(negotiation_hold, "answer");
        let answer = pc.create_answer(None).await.context("create answer")?;
        let sdp = answer.sdp.clone();
        negotiation_checkpoint!(negotiation_hold, "answer_local");
        pc.set_local_description(answer)
            .await
            .context("set local description (answer)")?;
        Ok(sdp)
    }

    /// Initiator path: apply the remote answer; flush buffered candidates.
    pub async fn handle_answer(&mut self, peer: PlayerId, sdp: String) -> Result<()> {
        #[cfg(test)]
        let negotiation_hold = self.negotiation_hold.clone();
        let link = self
            .peers
            .get_mut(&peer)
            .ok_or_else(|| anyhow!("answer from unpaired peer {peer}"))?;
        let answer = RTCSessionDescription::answer(sdp).context("parse remote answer SDP")?;
        let pc = link.pc.clone();
        negotiation_checkpoint!(negotiation_hold, "remote_answer");
        pc.set_remote_description(answer)
            .await
            .context("set remote description (answer)")?;
        link.remote_description_set = true;
        let pending = std::mem::take(&mut link.pending_candidates);
        for candidate in pending {
            negotiation_checkpoint!(negotiation_hold, "buffered_ice");
            if pc.add_ice_candidate(candidate).await.is_err() {
                tracing::warn!(%peer, "WebRTC stack rejected a buffered ICE candidate");
            }
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
        #[cfg(test)]
        let negotiation_hold = self.negotiation_hold.clone();
        if candidate_payload.len() > MAX_CANDIDATE_PAYLOAD_BYTES {
            return Err(anyhow!("remote ICE candidate payload exceeds 4096 bytes"));
        }
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
        if link.remote_candidate_count >= MAX_REMOTE_CANDIDATES
            || candidate_payload.len()
                > MAX_REMOTE_CANDIDATE_BYTES.saturating_sub(link.remote_candidate_bytes)
        {
            return Err(anyhow!("remote ICE candidate budget exhausted"));
        }
        link.remote_candidate_count = link.remote_candidate_count.saturating_add(1);
        link.remote_candidate_bytes = link
            .remote_candidate_bytes
            .saturating_add(candidate_payload.len());
        if link.remote_description_set {
            let pc = link.pc.clone();
            negotiation_checkpoint!(negotiation_hold, "live_ice");
            pc.add_ice_candidate(init)
                .await
                .context("apply remote candidate")?;
        } else {
            link.pending_candidates.push(init);
        }
        negotiation_checkpoint!(negotiation_hold, "candidate_complete");
        Ok(())
    }

    /// Accept the first required channel and close extras or distinct duplicates.
    /// Accepted remote channels start polling after admission.
    pub async fn store_remote_channel(
        &mut self,
        peer: PlayerId,
        label: String,
        channel: Arc<dyn DataChannel>,
    ) {
        if !required_channel_delivery_mode(&label, channel.as_ref()).await {
            let _ = channel.close().await;
            return;
        }
        let Some(link) = self.peers.get_mut(&peer) else {
            tracing::warn!(%peer, "remote channel announced for unknown peer");
            let _ = channel.close().await;
            return;
        };
        let selection = link.channel_admission.lock().await.select(&label, &channel);
        if selection == ChannelSelection::Duplicate {
            let _ = channel.close().await;
            return;
        }
        // WebRTC labels are not unique. Preserve the first selected channel.
        if let Some(existing) = link.channels.get(&label) {
            if Arc::ptr_eq(existing, &channel) {
                return;
            }
            if let Err(error) = channel.close().await {
                tracing::warn!(%peer, %error, "failed to close duplicate data channel");
            }
            return;
        }
        link.channels.insert(label.clone(), channel.clone());
        // Poll only accepted channels, so refused channels cannot publish open,
        // message, or close callbacks under a selected channel's label.
        link.channel_tasks.push(spawn_channel_event_loop(
            &self.runtime,
            &self.events,
            peer,
            link.generation,
            label,
            channel,
            link.receive_admission.clone(),
        ));
    }

    /// Record an open callback and inspect both selected channels.
    /// Returns `true` once per link when both channels are observed open.
    /// Callers must await this method. These observations do not prevent a
    /// later channel close; callers must still handle terminal channel events.
    pub async fn note_channel_open(&mut self, peer: PlayerId, label: &str) -> bool {
        let Some(link) = self.peers.get_mut(&peer) else {
            tracing::warn!(%peer, label, "channel open for unknown peer");
            return false;
        };
        if !link
            .receive_admission
            .allows_work(&self.events, peer, link.generation, None)
            .await
        {
            return false;
        }
        link.open_labels.insert(label.to_string());
        let fully_open = link.open_labels.contains(RELIABLE_LABEL)
            && link.open_labels.contains(UNRELIABLE_LABEL);
        if !fully_open || link.pair_connected {
            return false;
        }
        /*
            Queued open callbacks can outlive their channels. Separate state
            observations cannot prevent a later close.
        */
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            let Some(channel) = link.channels.get(label) else {
                return false;
            };
            if !matches!(
                channel.ready_state().await,
                Ok(rtc::data_channel::RTCDataChannelState::Open)
            ) {
                return false;
            }
        }
        if link.receive_admission.terminal.load(Ordering::Relaxed) {
            return false;
        }
        link.pair_connected = true;
        true
    }

    /// Look up a stored channel by peer and label.
    pub fn channel(&self, peer: PlayerId, label: &str) -> Option<Arc<dyn DataChannel>> {
        self.peers
            .get(&peer)
            .and_then(|link| link.channels.get(label))
            .cloned()
    }

    pub(crate) async fn send_channel(
        &self,
        peer: PlayerId,
        label: &str,
    ) -> Option<Arc<dyn DataChannel>> {
        let link = self.peers.get(&peer)?;
        if !link
            .receive_admission
            .allows_work(&self.events, peer, link.generation, None)
            .await
        {
            return None;
        }
        link.channels.get(label).cloned()
    }

    pub(crate) fn report_peer_loss(&self, peer: PlayerId, reason: &str) {
        if self.is_paired(peer) {
            tracing::warn!(
                %peer,
                %reason,
                snapshot = %self.diagnostic_snapshot(peer),
                "P2P link lost"
            );
        }
    }

    /// Local observations for a timed-out link, without waiting on its driver.
    /// An observed open callback does not establish current channel state.
    pub(crate) fn diagnostic_snapshot(&self, peer: PlayerId) -> serde_json::Value {
        let Some(link) = self.peers.get(&peer) else {
            return serde_json::Value::Null;
        };
        let channels: serde_json::Map<String, serde_json::Value> =
            [RELIABLE_LABEL, UNRELIABLE_LABEL]
                .into_iter()
                .map(|label| {
                    (
                        label.to_string(),
                        serde_json::json!({
                            "registered": link.channels.contains_key(label),
                            "open_observed": link.open_labels.contains(label),
                        }),
                    )
                })
                .collect();
        serde_json::json!({
            "generation": link.generation,
            "remote_description_set": link.remote_description_set,
            "remote_candidate_count": link.remote_candidate_count,
            "pending_remote_candidates": link.pending_candidates.len(),
            "channels": channels,
        })
    }

    /// Start a detached nominated-pair probe for the current physical
    /// link. Results use a dedicated channel so the orchestrator can arbitrate
    /// already-completed evidence at its soft deadline without consuming or
    /// reordering ordinary engine callbacks.
    pub fn start_selected_candidate_pair_probe(
        &self,
        peer: PlayerId,
        results: mpsc::UnboundedSender<SelectedPairProbeResult>,
    ) -> bool {
        let Some(link) = self.peers.get(&peer) else {
            return false;
        };
        let pc = link.pc.clone();
        let generation = link.generation;
        spawn_selected_pair_probe(
            self.runtime.clone(),
            results,
            peer,
            generation,
            async move {
                let nominated = pc
                    .sctp()
                    .await?
                    .transport()
                    .ice_transport()
                    .get_selected_candidate_pair()
                    .await
                    .ok()
                    .flatten();
                selected_candidate_pair_details(nominated.as_ref())
            },
        );
        true
    }
}

fn spawn_selected_pair_probe(
    runtime: Arc<dyn Runtime>,
    results: mpsc::UnboundedSender<SelectedPairProbeResult>,
    peer: PlayerId,
    generation: u64,
    probe: impl std::future::Future<Output = Option<SelectedCandidatePair>> + Send + 'static,
) {
    runtime.spawn(Box::pin(async move {
        let selected = probe.await;
        let _ = results.send(SelectedPairProbeResult {
            peer,
            generation,
            completed_at: tokio::time::Instant::now(),
            selected,
        });
    }));
}

/// Read the nominated pair itself, including dynamically learned candidates.
/// The statistics registry can omit a peer-reflexive remote candidate.
fn selected_candidate_pair_details(
    nominated: Option<&RTCIceCandidatePair>,
) -> Option<SelectedCandidatePair> {
    let pair = nominated?;
    Some(SelectedCandidatePair {
        local_candidate_type: pair.local().typ.to_string(),
        remote_candidate_type: pair.remote().typ.to_string(),
        local_candidate_address: Some(pair.local().address.clone()),
        remote_candidate_address: Some(pair.remote().address.clone()),
    })
}

/// A local ICE candidate this client gathered and advertised to a peer.
///
/// Taken from the stack's typed candidate rather than re-parsed out of the SDP
/// attribute line, so the fields cannot drift from what was actually
/// advertised. A harness asserts on the advertised *set*: that a relay-only
/// session gathered a relay candidate at all (issue #276 had none, and the
/// only evidence was "no candidate pairs"), and that an `--ip-family` run
/// advertised nothing of the other family (issue #275).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatheredCandidate {
    /// `host`, `srflx`, `prflx` or `relay`.
    pub candidate_type: String,
    /// The candidate's own address, as the stack renders it.
    pub address: String,
    pub port: u16,
    /// `udp` or `tcp`.
    pub protocol: String,
}

/// The ICE candidate pair that carries a connected peer's data channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedCandidatePair {
    pub local_candidate_type: String,
    pub remote_candidate_type: String,
    /// Actual local address from the nominated transport pair. The event
    /// schema remains optional; family-sensitive consumers reject absence.
    pub local_candidate_address: Option<String>,
    /// Address reported for the remote side of the pair.
    pub remote_candidate_address: Option<String>,
}

/// Completion from one detached nominated-pair transport probe.
pub struct SelectedPairProbeResult {
    pub peer: PlayerId,
    pub generation: u64,
    pub completed_at: tokio::time::Instant,
    pub selected: Option<SelectedCandidatePair>,
}

struct PeerHandler {
    peer: PlayerId,
    generation: u64,
    crippled: bool,
    events: mpsc::UnboundedSender<EngineEvent>,
    channel_admission: Arc<Mutex<RequiredChannelAdmission>>,
    receive_admission: Arc<ChannelReceiveAdmission>,
}

#[derive(Default)]
struct RequiredChannelAdmission {
    reliable: Option<Weak<dyn DataChannel>>,
    unreliable: Option<Weak<dyn DataChannel>>,
}

#[derive(PartialEq, Eq)]
enum ChannelSelection {
    First,
    Repeated,
    Duplicate,
}

impl RequiredChannelAdmission {
    fn select(&mut self, label: &str, channel: &Arc<dyn DataChannel>) -> ChannelSelection {
        let selected = match label {
            RELIABLE_LABEL => &mut self.reliable,
            UNRELIABLE_LABEL => &mut self.unreliable,
            _ => return ChannelSelection::Duplicate,
        };
        let candidate = Arc::downgrade(channel);
        match selected {
            Some(existing) if Weak::ptr_eq(existing, &candidate) => ChannelSelection::Repeated,
            Some(_) => ChannelSelection::Duplicate,
            None => {
                // Keep the identity after closure or queue drain. A fresh physical
                // link gets fresh slots; weak references do not retain peer state.
                *selected = Some(candidate);
                ChannelSelection::First
            }
        }
    }
}

#[async_trait]
impl PeerConnectionEventHandler for PeerHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if self.crippled {
            return;
        }
        let gathered = GatheredCandidate {
            candidate_type: event.candidate.typ.to_string(),
            address: event.candidate.address.clone(),
            port: event.candidate.port,
            protocol: event.candidate.protocol.to_string(),
        };
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
                    gathered,
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
        if matches!(
            state,
            RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
        ) {
            self.receive_admission
                .terminal
                .store(true, Ordering::Relaxed);
        }
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
                let _ = channel.close().await;
                return;
            }
        };
        // Refuse unsupported channels before the unbounded orchestrator queue.
        // They need no retained label, polling task, or application callbacks.
        if !required_channel_delivery_mode(&label, channel.as_ref()).await {
            let _ = channel.close().await;
            return;
        }
        let selection = self.channel_admission.lock().await.select(&label, &channel);
        match selection {
            ChannelSelection::Repeated => return,
            ChannelSelection::Duplicate => {
                let _ = channel.close().await;
                return;
            }
            ChannelSelection::First => {}
        }
        if let Err(error) = self.events.send(EngineEvent::RemoteChannel {
            peer: self.peer,
            generation: self.generation,
            label,
            channel,
        }) {
            if let EngineEvent::RemoteChannel { channel, .. } = error.0 {
                let _ = channel.close().await;
            }
        }
    }
}

// Labels do not establish ordering or reliability. Refuse invalid modes before
// reserving a required label or publishing any application callback. A closed
// channel's failed property read also fails admission.
async fn required_channel_delivery_mode(label: &str, channel: &dyn DataChannel) -> bool {
    let (ordered, retransmits) = match label {
        RELIABLE_LABEL => (true, None),
        UNRELIABLE_LABEL => (false, Some(0)),
        _ => return false,
    };
    matches!(channel.ordered().await, Ok(value) if value == ordered)
        && matches!(channel.max_retransmits().await, Ok(value) if value == retransmits)
        && matches!(channel.max_packet_life_time().await, Ok(None))
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
///
/// `resolve` is the bind-selection rule; production passes [`local_udp_addrs`]
/// and a test passes a fixed interface set, so the failure path is reachable
/// on a host that happens to serve every family. There is deliberately no
/// convenience wrapper that hard-codes the resolver: a wrapper would be a
/// seam no host-independent test can cover.
pub fn preflight_ip_family(
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

/// The complete bind set for one peer connection: every address the interface
/// table offers, plus the local address the host's own routing table selects
/// for each configured ICE server.
///
/// Interface enumeration alone is not sufficient. webrtc 0.20 starts a STUN
/// binding and a TURN allocation from *every* socket the application supplies,
/// and a socket that cannot route to the server either fails outright at
/// `sendto` (`EINVAL` from a loopback source toward a routed destination) or is
/// dropped in transit. When no bound address routes to the server, the run
/// gathers no relay candidate — and under `--ice-transport-policy relay`, no
/// candidate at all, which is issue #276. The kernel's own source address for
/// a server is routable by construction, so it belongs in the bind set whether
/// or not interface enumeration happened to report it.
///
/// `enumerate` is the interface rule (production passes [`local_udp_addrs`])
/// and `sources` the routing-table answers (production passes
/// [`ice_server_source_addrs`]). Both halves of the union are arguments of the
/// production entry point, so the unit-tested function *is* the shipped one.
pub fn session_udp_addrs(
    settings: EngineSettings,
    enumerate: impl Fn(EngineSettings) -> Result<Vec<SocketAddr>>,
    sources: impl IntoIterator<Item = IpAddr>,
) -> Result<Vec<SocketAddr>> {
    let enumerated = enumerate(settings)?;
    if settings.crippled {
        // The crippled transport exists to be unusable. A routable source
        // address would hand it exactly the reachability it must never have.
        return Ok(enumerated);
    }
    // One rule governs both halves: `select_udp_addrs` drops what cannot serve
    // as a host candidate and enforces `--ip-family`, so a routing answer can
    // never smuggle in an address the family pin excludes.
    let merged = select_udp_addrs(
        enumerated.iter().map(SocketAddr::ip).chain(sources),
        settings.ip_family,
    )?;
    // Report the resolved bind set unconditionally. Reporting only the union's
    // additions leaves the interesting failure invisible: a run where the probe
    // contributed nothing looks exactly like a run where it was never
    // consulted, and a failing lane then shows only "no candidate pairs" with
    // nothing naming which addresses ICE was actually given. `added` is
    // computed by membership rather than by comparing lengths, because the
    // interface rule drops link-local and duplicate addresses, so a set that
    // gained a routing answer can still be no larger.
    let enumerated_ips: BTreeSet<IpAddr> = enumerated.iter().map(SocketAddr::ip).collect();
    let bound: Vec<IpAddr> = merged.iter().map(SocketAddr::ip).collect();
    let added: Vec<IpAddr> = bound
        .iter()
        .copied()
        .filter(|ip| !enumerated_ips.contains(ip))
        .collect();
    tracing::info!(
        ?bound,
        enumerated = ?enumerated_ips,
        ?added,
        family = settings.ip_family.as_str(),
        "resolved the ICE bind set for this peer connection"
    );
    if !added.is_empty() {
        // Name the exact condition behind issue #276.
        tracing::info!(
            ?added,
            "interface enumeration missed a route to a configured ICE server; \
             binding the kernel's source address for it as well"
        );
    }
    Ok(merged)
}

/// Total budget for the routing probe.
///
/// The probe runs on the orchestrator task, which also pumps the WebSocket, so
/// a resolver that hangs must cost the session a bounded delay and the union —
/// never the pairing, and never the server's activity deadlines. Every
/// configured URL shares one budget; on expiry the enumerated interface
/// addresses stand alone, exactly as before this rule existed.
const ICE_ROUTE_PROBE_BUDGET: Duration = Duration::from_secs(2);

/// Kernel-selected source addresses for the given STUN/TURN endpoints.
///
/// A server this host cannot resolve or route to contributes nothing and is
/// reported as a diagnostic rather than an error: the enumerated interface
/// addresses still stand, and webrtc applies its own URL validation to the
/// same list.
pub async fn ice_server_source_addrs(endpoints: &[(String, u16)]) -> Vec<IpAddr> {
    if endpoints.is_empty() {
        return Vec::new();
    }
    let probe = async {
        let mut sources = BTreeSet::new();
        for (host, port) in endpoints {
            let resolved = match tokio::net::lookup_host((host.as_str(), *port)).await {
                Ok(resolved) => resolved,
                Err(error) => {
                    // A configured server this host cannot even name is a real
                    // operational condition, not a detail: every allocation
                    // through it will fail, and the session's only remaining
                    // hope is that interface enumeration happens to cover the
                    // route. It must be visible at the level a failing run's
                    // captured stderr actually records.
                    tracing::warn!(%host, port, %error, "ICE server did not resolve");
                    continue;
                }
            };
            for server in resolved {
                match route_source_addr(server) {
                    Ok(source) => {
                        tracing::info!(%server, %source, "routed a configured ICE server");
                        sources.insert(source);
                    }
                    Err(error) => {
                        tracing::warn!(%server, %error, "no local route to this ICE server");
                    }
                }
            }
        }
        sources.into_iter().collect()
    };
    route_sources_within(ICE_ROUTE_PROBE_BUDGET, probe).await
}

/// Run `probe` under `budget`, yielding no routing answers when it expires.
///
/// Separated from the probe body so the expiry path is reachable from a test
/// without a hung resolver. A `timeout` wrapped around real resolution races
/// Tokio's poll order — the inner future is polled before the timer, so a
/// lookup that already finished on the blocking pool wins — which is a flaky
/// oracle, not a proof.
async fn route_sources_within(
    budget: Duration,
    probe: impl std::future::Future<Output = Vec<IpAddr>>,
) -> Vec<IpAddr> {
    match tokio::time::timeout(budget, probe).await {
        Ok(sources) => sources,
        Err(_elapsed) => {
            tracing::warn!(
                budget_ms = budget.as_millis(),
                "ICE server route probe exceeded its budget; pairing continues on the \
                 enumerated interface addresses alone"
            );
            Vec::new()
        }
    }
}

/// Host and port of every STUN or TURN URL a session plan carried.
///
/// Unparsable or non-STUN/TURN URLs are skipped rather than rejected: this
/// client is not the authority on the URL grammar, and an entry it cannot read
/// must not cost the session the servers it can.
pub fn ice_server_endpoints(ice_servers: &[IceServer]) -> Vec<(String, u16)> {
    let mut endpoints = BTreeSet::new();
    for server in ice_servers {
        for raw in &server.urls {
            match Url::parse_url(raw) {
                Ok(url)
                    if matches!(
                        url.scheme,
                        SchemeType::Stun | SchemeType::Stuns | SchemeType::Turn | SchemeType::Turns
                    ) =>
                {
                    endpoints.insert((url.host.clone(), url.port));
                }
                Ok(url) => {
                    tracing::debug!(%raw, scheme = %url.scheme, "ignoring non-STUN/TURN ICE URL");
                }
                Err(error) => {
                    tracing::debug!(%raw, %error, "ignoring unparsable ICE URL");
                }
            }
        }
    }
    endpoints.into_iter().collect()
}

/// The local address this host would send from to reach `server`, as chosen by
/// its routing table.
///
/// Connecting an unbound UDP socket performs the route lookup and assigns the
/// source address without sending a packet, which is the portable way to ask
/// "which of my addresses reaches this peer?" on Linux, macOS and Windows.
fn route_source_addr(server: SocketAddr) -> std::io::Result<IpAddr> {
    let wildcard = if server.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    let probe = UdpSocket::bind(wildcard)?;
    probe.connect(server)?;
    Ok(probe.local_addr()?.ip())
}

fn spawn_channel_event_loop(
    runtime: &Arc<dyn Runtime>,
    events: &mpsc::UnboundedSender<EngineEvent>,
    peer: PlayerId,
    generation: u64,
    label: String,
    channel: Arc<dyn DataChannel>,
    admission: Arc<ChannelReceiveAdmission>,
) -> Box<dyn JoinHandle> {
    let events = events.clone();
    runtime.spawn(Box::pin(async move {
        while let Some(event) = channel.poll().await {
            // Admission can suspend; publication time must not replace arrival.
            let received_at = tokio::time::Instant::now();
            #[cfg(test)]
            if matches!(event, DataChannelEvent::OnMessage(_)) {
                if let Ok(observed) = admission.message_observed.lock() {
                    if let Some(observed) = observed.as_ref() {
                        observed.notify_one();
                    }
                }
            }
            let mut rejected = admission.rejected.lock().await;
            if *rejected {
                return;
            }
            match event {
                DataChannelEvent::OnOpen => {
                    let _ = events.send(EngineEvent::ChannelOpen {
                        peer,
                        generation,
                        label: label.clone(),
                    });
                }
                DataChannelEvent::OnMessage(message) => {
                    /*
                        Prior observed loss stops new input admission while
                        lifecycle events still drain. Loss can race this check.
                    */
                    if !admission
                        .allows_work(&events, peer, generation, Some(&label))
                        .await
                    {
                        continue;
                    }
                    let Some((text, queue_permit)) = admission.decode_and_reserve(&message.data)
                    else {
                        *rejected = true;
                        admission.terminal.store(true, Ordering::Relaxed);
                        let _ = events.send(EngineEvent::ChannelReceiveRejected {
                            peer,
                            generation,
                            label: label.clone(),
                        });
                        drop(rejected);
                        let _ = channel.close().await;
                        return;
                    };
                    if events
                        .send(EngineEvent::ChannelMessage {
                            peer,
                            generation,
                            label: label.clone(),
                            text,
                            queue_permit,
                            received_at,
                        })
                        .is_err()
                    {
                        drop(rejected);
                        let _ = channel.close().await;
                        return;
                    }
                }
                DataChannelEvent::OnError => {
                    admission.terminal.store(true, Ordering::Relaxed);
                    tracing::warn!(%peer, channel = %label, "data channel became unreadable");
                    break;
                }
                DataChannelEvent::OnClosing | DataChannelEvent::OnClose => {
                    admission.terminal.store(true, Ordering::Relaxed);
                    break;
                }
                DataChannelEvent::OnBufferedAmountLow | DataChannelEvent::OnBufferedAmountHigh => {}
                // The 0.21 event enum is `#[non_exhaustive]`; unknown future
                // driver events stay visible no-ops instead of failing the
                // client build.
                other => {
                    tracing::debug!(
                        ?other,
                        %peer,
                        channel = %label,
                        "unhandled data channel event"
                    );
                }
            }
        }
        /*
            Driver shutdown can end streams without OnClose. Shared receive
            rejection already publishes the link's terminal event.
        */
        admission.terminal.store(true, Ordering::Relaxed);
        let rejected = admission.rejected.lock().await;
        if !*rejected {
            admission.report_channel_closed(&events, peer, generation, &label);
        }
    }))
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
#[path = "engine_task_tests.rs"]
mod task_tests;

#[cfg(test)]
#[path = "engine_admission_tests.rs"]
mod admission_tests;

#[cfg(test)]
#[path = "engine_receive_tests.rs"]
mod receive_tests;

#[cfg(test)]
#[path = "engine_close_tests.rs"]
mod close_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_candidate_limits_preserve_accepted_signaling() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(crippled_settings(IpFamily::Ipv4), tx).unwrap();
        let mut json_payload = r#"{"candidate":"","usernameFragment":""}"#.to_string();
        json_payload.insert_str(
            json_payload.len() - 2,
            &"x".repeat(4096 - json_payload.len()),
        );
        for (case, payload, accepted) in [
            (
                "count",
                "candidate:1 1 udp 2130706431 127.0.0.1 9 typ host".to_string(),
                128,
            ),
            ("bytes", "x".repeat(4096), 16),
            ("utf8", "é".repeat(2048), 16),
            ("json-metadata", json_payload, 16),
        ] {
            let peer = PlayerId::new_v4();
            engine.pair_with(peer, false, &[]).await.unwrap();
            assert!(
                engine
                    .handle_remote_candidate(peer, &"x".repeat(4097))
                    .await
                    .is_err(),
                "{case}: oversized input"
            );
            assert!(
                engine
                    .handle_remote_candidate(peer, &"é".repeat(2049))
                    .await
                    .is_err(),
                "{case}: oversized UTF-8 input"
            );
            assert!(
                engine.peers[&peer].pending_candidates.is_empty(),
                "{case}: rejected input must not be retained"
            );
            for _ in 0..accepted {
                engine
                    .handle_remote_candidate(peer, &payload)
                    .await
                    .unwrap();
            }
            for _ in 0..3 {
                assert!(
                    engine
                        .handle_remote_candidate(peer, &payload)
                        .await
                        .is_err(),
                    "{case}: exhausted candidate budget"
                );
            }
            let link = &engine.peers[&peer];
            assert_eq!(
                link.pending_candidates.len(),
                accepted,
                "{case}: preserve bounded accepted candidates"
            );
            let expected_candidate = serde_json::from_str::<RTCIceCandidateInit>(&payload)
                .map_or_else(|_| payload.clone(), |init| init.candidate);
            assert!(link
                .pending_candidates
                .iter()
                .all(|candidate| candidate.candidate == expected_candidate));
            // Queue drain must not renew the WebRTC stack's admission budget.
            let link = engine.peers.get_mut(&peer).unwrap();
            link.remote_description_set = true;
            link.pending_candidates.clear();
            let error = engine
                .handle_remote_candidate(peer, &payload)
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), "remote ICE candidate budget exhausted");
            let other = PlayerId::new_v4();
            engine.pair_with(other, false, &[]).await.unwrap();
            engine
                .handle_remote_candidate(other, &payload)
                .await
                .unwrap();
            engine.remove_peer(other).await.unwrap();
            engine.remove_peer(peer).await.unwrap();
            engine.pair_with(peer, false, &[]).await.unwrap();
            engine
                .handle_remote_candidate(peer, &payload)
                .await
                .unwrap();
            assert_eq!(
                engine.peers[&peer].pending_candidates.len(),
                1,
                "{case}: replacement gets a fresh budget"
            );
            engine.remove_peer(peer).await.unwrap();
        }
    }

    // Regression #861: use real SDP and the WebRTC stack on both sides of
    // the pending-queue drain. The last admission must not renew that queue.
    #[tokio::test]
    async fn remote_candidate_budget_survives_real_description_application() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let settings = crippled_settings(IpFamily::Ipv4);
        let mut offerer = Engine::new(settings, tx.clone()).unwrap();
        let mut answerer = Engine::new(settings, tx).unwrap();
        let a = PlayerId::new_v4();
        let b = PlayerId::new_v4();
        let offer = offerer.pair_with(b, true, &[]).await.unwrap().unwrap();
        answerer.pair_with(a, false, &[]).await.unwrap();
        let payload = r#"{"candidate":"candidate:1 1 udp 2130706431 127.0.0.1 9 typ host","sdpMid":"0","sdpMLineIndex":0,"usernameFragment":null}"#;
        for _ in 0..127 {
            answerer.handle_remote_candidate(a, payload).await.unwrap();
        }
        assert_eq!(answerer.peers[&a].pending_candidates.len(), 127);
        assert_eq!(
            answerer.peers[&a].pending_candidates[0].sdp_mline_index,
            Some(0)
        );
        let answer = answerer.handle_offer(a, offer).await.unwrap();
        assert!(answerer.peers[&a].pending_candidates.is_empty());
        answerer.handle_remote_candidate(a, payload).await.unwrap();
        assert!(answerer.peers[&a].pending_candidates.is_empty());
        let error = answerer
            .handle_remote_candidate(a, payload)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "remote ICE candidate budget exhausted");
        offerer.handle_answer(b, answer).await.unwrap();
        offerer.remove_peer(b).await.unwrap();
        answerer.remove_peer(a).await.unwrap();
    }

    // Real SDP and remote-candidate stats prove that a rejected queued
    // candidate cannot prevent the answer or discard later valid candidates.
    #[tokio::test]
    async fn buffered_candidate_rejection_preserves_offer_negotiation() {
        buffered_candidate_recovery(false).await;
    }

    #[tokio::test]
    async fn buffered_candidate_rejection_preserves_answer_negotiation() {
        buffered_candidate_recovery(true).await;
    }

    async fn buffered_candidate_recovery(answer_path: bool) {
        const INVALID: &str = "candidate:invalid";
        const FIRST: &str = "candidate:1 1 udp 2130706431 127.0.0.1 50001 typ host";
        const SECOND: &str = "candidate:2 1 udp 2130706430 127.0.0.1 50002 typ host";
        const LATER: &str = "candidate:3 1 udp 2130706429 127.0.0.1 50003 typ host";
        for (case, candidates, expected_ports) in [
            ("empty", vec![], BTreeSet::new()),
            (
                "normal",
                vec![FIRST, SECOND],
                BTreeSet::from([50001, 50002]),
            ),
            ("invalid only", vec![INVALID], BTreeSet::new()),
            (
                "invalid first",
                vec![INVALID, FIRST, SECOND],
                BTreeSet::from([50001, 50002]),
            ),
            (
                "invalid middle",
                vec![FIRST, INVALID, SECOND],
                BTreeSet::from([50001, 50002]),
            ),
        ] {
            let (tx, _rx) = mpsc::unbounded_channel();
            let settings = mdns_disabled_settings();
            let mut offerer = Engine::new(settings, tx.clone()).unwrap();
            let mut answerer = Engine::new(settings, tx).unwrap();
            let a = PlayerId::new_v4();
            let b = PlayerId::new_v4();
            let offer = offerer.pair_with(b, true, &[]).await.unwrap().unwrap();
            answerer.pair_with(a, false, &[]).await.unwrap();
            let (engine, peer, remote_sdp) = if answer_path {
                let answer = answerer.handle_offer(a, offer).await.unwrap();
                (&mut offerer, b, answer)
            } else {
                (&mut answerer, a, offer)
            };
            for candidate in &candidates {
                engine
                    .handle_remote_candidate(peer, candidate)
                    .await
                    .unwrap();
            }
            let admitted_bytes: usize = candidates.iter().map(|candidate| candidate.len()).sum();
            if answer_path {
                engine
                    .handle_answer(peer, remote_sdp)
                    .await
                    .unwrap_or_else(|error| panic!("{case}: apply answer: {error:#}"));
            } else {
                let answer = engine
                    .handle_offer(peer, remote_sdp)
                    .await
                    .unwrap_or_else(|error| panic!("{case}: produce answer: {error:#}"));
                assert!(!answer.is_empty(), "{case}: responder produces an answer");
                offerer.handle_answer(b, answer).await.unwrap();
            }
            let engine = if answer_path {
                &mut offerer
            } else {
                &mut answerer
            };
            let link = &engine.peers[&peer];
            assert!(link.remote_description_set, "{case}: description applied");
            assert!(link.pending_candidates.is_empty(), "{case}: queue drained");
            assert_eq!(
                link.remote_candidate_count,
                candidates.len(),
                "{case}: admission count retained"
            );
            assert_eq!(
                link.remote_candidate_bytes, admitted_bytes,
                "{case}: admission bytes retained"
            );
            assert_eq!(
                remote_candidate_ports(link).await,
                expected_ports,
                "{case}: all valid queued candidates reach the ICE stack"
            );
            assert!(
                engine.handle_remote_candidate(peer, INVALID).await.is_err(),
                "{case}: later invalid candidates remain isolated"
            );
            engine.handle_remote_candidate(peer, LATER).await.unwrap();
            let mut expected_ports = expected_ports;
            expected_ports.insert(50003);
            assert_eq!(
                remote_candidate_ports(&engine.peers[&peer]).await,
                expected_ports,
                "{case}: signaling continues after flush"
            );
            offerer.remove_peer(b).await.unwrap();
            answerer.remove_peer(a).await.unwrap();
        }
    }

    async fn remote_candidate_ports(link: &PeerLink) -> BTreeSet<u16> {
        link.pc
            .get_stats(std::time::Instant::now(), StatsSelector::None)
            .await
            .iter()
            .filter_map(|entry| match entry {
                RTCStatsReportEntry::RemoteCandidate(candidate) => Some(candidate.port),
                _ => None,
            })
            .collect()
    }

    /// Payload of the routing probes below. Its exact bytes are compared on
    /// arrival, so a datagram from anything else cannot be mistaken for it.
    const ROUTE_PROBE: &[u8] = b"signal-fish-route-probe";

    #[tokio::test]
    async fn detached_selected_pair_probe_cannot_block_unrelated_work() {
        let runtime = default_runtime().expect("test runtime is available");
        let (result_tx, mut result_rx) = mpsc::unbounded_channel();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let peer = PlayerId::from_u128(0x05E1_EC7E_D006);

        spawn_selected_pair_probe(runtime, result_tx, peer, 9, async move {
            started_tx
                .send(())
                .expect("test observes the probe reaching its pending point");
            release_rx.await.expect("test releases the hanging probe");
            Some(SelectedCandidatePair {
                local_candidate_type: "host".to_string(),
                remote_candidate_type: "host".to_string(),
                local_candidate_address: Some(Ipv4Addr::LOCALHOST.to_string()),
                remote_candidate_address: Some(Ipv4Addr::LOCALHOST.to_string()),
            })
        });

        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx)
            .await
            .expect("detached probe is polled promptly")
            .expect("detached probe reaches its deliberate hang");
        let unrelated_gameplay_work = async { "gameplay-ready" }.await;
        assert_eq!(unrelated_gameplay_work, "gameplay-ready");
        match result_rx.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                panic!("the deliberately hanging diagnostic task stays alive")
            }
            Ok(_) => panic!("the deliberately hanging diagnostic stays detached"),
        }

        release_tx.send(()).expect("probe task remains alive");
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), result_rx.recv())
            .await
            .expect("released probe publishes promptly")
            .expect("probe channel remains open");
        assert_eq!(result.peer, peer);
        assert_eq!(result.generation, 9);
        assert!(result.selected.is_some());
    }

    /// Issue #892: report real nominated candidate types and addresses.
    #[test]
    fn nominated_pair_evidence_preserves_peer_reflexive_addresses_and_types() {
        use webrtc::peer_connection::{RTCIceCandidate, RTCIceCandidateType};
        assert!(selected_candidate_pair_details(None).is_none());
        for (local_type, remote_type) in [
            (RTCIceCandidateType::Host, RTCIceCandidateType::Host),
            (RTCIceCandidateType::Host, RTCIceCandidateType::Prflx),
            (RTCIceCandidateType::Relay, RTCIceCandidateType::Relay),
            (RTCIceCandidateType::Srflx, RTCIceCandidateType::Prflx),
        ] {
            let pair = RTCIceCandidatePair::new(
                RTCIceCandidate {
                    typ: local_type,
                    address: "::1".to_string(),
                    ..RTCIceCandidate::default()
                },
                RTCIceCandidate {
                    typ: remote_type,
                    address: "2001:db8::7".to_string(),
                    ..RTCIceCandidate::default()
                },
            );
            let evidence = selected_candidate_pair_details(Some(&pair)).unwrap();
            assert_eq!(evidence.local_candidate_type, local_type.to_string());
            assert_eq!(evidence.remote_candidate_type, remote_type.to_string());
            assert_eq!(evidence.local_candidate_address.as_deref(), Some("::1"));
            assert_eq!(
                evidence.remote_candidate_address.as_deref(),
                Some("2001:db8::7")
            );
        }
    }

    /// Bind each candidate address and try to send [`ROUTE_PROBE`] to `server`,
    /// reporting what the kernel did for every one of them.
    ///
    /// This is the operation issue #276 observed failing in production: webrtc
    /// starts its TURN allocation from each bound socket, and a socket that
    /// cannot route to the server fails here (`EINVAL` for a loopback source
    /// toward a routed destination) rather than at any protocol layer.
    fn probe_route(binds: &[SocketAddr], server: SocketAddr) -> Vec<(SocketAddr, String)> {
        binds
            .iter()
            .map(|bind| {
                let outcome = UdpSocket::bind(*bind)
                    .and_then(|socket| socket.send_to(ROUTE_PROBE, server))
                    .map_or_else(|error| format!("{error}"), |_sent| "sent".to_string());
                (*bind, outcome)
            })
            .collect()
    }

    /// The first bind address whose socket reached `server`, if any.
    fn source_that_reaches(binds: &[SocketAddr], server: SocketAddr) -> Option<SocketAddr> {
        probe_route(binds, server)
            .into_iter()
            .find(|(_bind, outcome)| outcome == "sent")
            .map(|(bind, _outcome)| bind)
    }

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
            "the crippled transport exists to be unusable; --ip-family must not \
             fork it into a shape nothing exercises"
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
        let error = preflight_ip_family(requested, unservable)
            .expect_err("an unservable family must fail the pre-flight");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("--ip-family ipv6"),
            "the failure must name the flag that caused it: {rendered}"
        );

        // The default never consults the resolver at all, so a host with no
        // usable interface still fails later (per pair) exactly as before.
        preflight_ip_family(EngineSettings::default(), |_settings| {
            panic!("--ip-family any must not be resolved eagerly")
        })
        .expect("the default family imposes no startup requirement");

        // No `preflight_ip_family(_, local_udp_addrs)` cross-check here: for a
        // non-`Any` family the pre-flight IS `resolve(settings)`, so comparing
        // the two would restate the definition. That this is the production
        // entry point is a property of the call site, pinned in
        // `tests/ci_config_tests.rs`.
    }

    #[test]
    fn the_bind_set_reaches_a_configured_server_the_interface_table_misses() {
        // Stand-in for the configured TURN server: a socket this test owns, on
        // the IPv6 loopback every supported platform provides. Using a real
        // socket means "reachable" is decided by the kernel, not by the test.
        let server = UdpSocket::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, 0)))
            .expect("the IPv6 loopback must be bindable");
        let server_addr = server.local_addr().expect("a bound socket has an address");
        server
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("probe reads must not block a failing run forever");

        // Run 30962028644's condition reduced to its essence: interface
        // enumeration produced no address that can reach the configured
        // server. There, every Allocate either failed with `EINVAL` from the
        // loopback source or was dropped before coturn, and the relay-only
        // session gathered no candidate at all.
        let missing_the_route = vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))];
        assert!(
            source_that_reaches(&missing_the_route, server_addr).is_none(),
            "precondition: the enumerated set must not already reach {server_addr}, \
             otherwise this proves nothing: {:?}",
            probe_route(&missing_the_route, server_addr)
        );

        let source = route_source_addr(server_addr)
            .expect("every host routes to its own loopback by definition");
        let bound = session_udp_addrs(
            EngineSettings::default(),
            |_settings| Ok(missing_the_route.clone()),
            [source],
        )
        .expect("merging one routing answer into a non-empty set resolves");

        // Every address the interface rule offered is still bound: host
        // candidates are not sacrificed to reach a TURN server.
        for addr in &missing_the_route {
            assert!(bound.contains(addr), "{addr} must stay in {bound:?}");
        }
        let reaching = source_that_reaches(&bound, server_addr).unwrap_or_else(|| {
            panic!(
                "the merged bind set still cannot reach {server_addr}: {:?}",
                probe_route(&bound, server_addr)
            )
        });

        // A successful `send_to` is not delivery. Read the datagram back, so a
        // kernel that accepted the write but dropped the packet still fails.
        let mut buffer = [0_u8; 64];
        let (read, from) = server
            .recv_from(&mut buffer)
            .expect("the probe datagram must arrive");
        assert_eq!(&buffer[..read], ROUTE_PROBE);
        assert_eq!(
            from.ip(),
            reaching.ip(),
            "the datagram must come from the address the merge added"
        );
    }

    #[tokio::test]
    async fn ice_server_source_addrs_routes_the_production_turn_url_shape() {
        let server = UdpSocket::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, 0)))
            .expect("the IPv6 loopback must be bindable");
        let port = server.local_addr().expect("bound socket").port();
        // Exactly the shape scripts/run-turn-interop.sh mints, so URL parsing,
        // resolution and the route probe are proven on the production string.
        let ice_servers = vec![IceServer {
            urls: vec![format!("turn:[::1]:{port}?transport=udp")],
            username: Some("interop".to_string()),
            credential: Some("interop".to_string()),
        }];
        assert_eq!(
            ice_server_source_addrs(&ice_server_endpoints(&ice_servers)).await,
            vec![IpAddr::from(Ipv6Addr::LOCALHOST)],
            "the routing answer for a reachable TURN URL is its own loopback"
        );

        // A server this host cannot resolve costs the session nothing.
        let unresolvable = [IceServer {
            urls: vec!["turn:invalid.invalid:3478?transport=udp".to_string()],
            username: None,
            credential: None,
        }];
        assert!(
            ice_server_source_addrs(&ice_server_endpoints(&unresolvable))
                .await
                .is_empty()
        );
        // No endpoints means no probe at all, not an empty resolution.
        assert!(ice_server_source_addrs(&[]).await.is_empty());
    }

    #[tokio::test]
    async fn the_route_probe_gives_up_within_its_budget() {
        // The probe runs on the task that also pumps the WebSocket, so a
        // resolver that never answers must cost the union, not the session: a
        // stalled read would let the server's own activity deadline close a
        // healthy connection. `pending()` IS that resolver, exactly, with no
        // dependence on when a blocking lookup happens to finish.
        assert_eq!(
            route_sources_within(Duration::from_millis(50), std::future::pending()).await,
            Vec::<IpAddr>::new(),
            "an expired budget yields no routing answers, never a stall"
        );
        // A probe that answers inside its budget is returned untouched, so the
        // assertion above cannot be passing because the wrapper drops answers.
        let answer = vec![IpAddr::from(Ipv4Addr::LOCALHOST)];
        assert_eq!(
            route_sources_within(Duration::from_secs(30), async { answer.clone() }).await,
            answer
        );
        // The shipped budget is the only thing between a hung resolver and a
        // stalled pairing; a zero or near-zero value would disable the union on
        // every host, silently, with no failing cell anywhere.
        assert!(
            ICE_ROUTE_PROBE_BUDGET >= Duration::from_secs(1),
            "the route probe budget must leave a real resolver time to answer"
        );
    }

    #[tokio::test]
    async fn routing_answers_are_probed_once_per_endpoint_set() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(EngineSettings::default(), tx).expect("engine builds");
        let loopback = [IceServer {
            urls: vec!["turn:127.0.0.1:3478?transport=udp".to_string()],
            username: Some("a".to_string()),
            credential: Some("b".to_string()),
        }];
        let expected = vec![IpAddr::from(Ipv4Addr::LOCALHOST)];
        assert_eq!(engine.ice_route_sources(&loopback).await, expected);

        // A credential rotation keeps the same URLs, so it must not re-probe.
        let rotated = [IceServer {
            urls: loopback[0].urls.clone(),
            username: Some("rotated".to_string()),
            credential: Some("rotated".to_string()),
        }];
        assert_eq!(engine.ice_route_sources(&rotated).await, expected);
        assert_eq!(
            engine.ice_route_sources.as_ref().map(|(probed, _)| probed),
            Some(&ice_server_endpoints(&loopback)),
            "the memo is keyed by the endpoint set, not by the credentials"
        );

        // A replan that changes the servers must, so a stale answer cannot
        // outlive the endpoints it was measured for.
        let replanned = [IceServer {
            urls: vec!["turn:[::1]:3478?transport=udp".to_string()],
            username: None,
            credential: None,
        }];
        assert_eq!(
            engine.ice_route_sources(&replanned).await,
            vec![IpAddr::from(Ipv6Addr::LOCALHOST)]
        );

        // A plan with no ICE servers probes nothing and caches that.
        assert!(engine.ice_route_sources(&[]).await.is_empty());
        assert_eq!(
            engine.ice_route_sources,
            Some((Vec::new(), Vec::new())),
            "an empty endpoint set is a cached answer, not a repeated no-op probe"
        );
    }

    #[test]
    fn ice_server_endpoints_reads_every_stun_and_turn_shape() {
        let cases: [(&str, Option<(&str, u16)>); 8] = [
            // The production TURN interop URL, and its IPv6 literal form.
            (
                "turn:10.254.124.2:3478?transport=udp",
                Some(("10.254.124.2", 3478)),
            ),
            (
                "turn:[2001:db8::1]:3478?transport=udp",
                Some(("2001:db8::1", 3478)),
            ),
            (
                "turns:turn.example.com:5349?transport=tcp",
                Some(("turn.example.com", 5349)),
            ),
            // Default ports differ by scheme and must not be invented here.
            ("stun:stun.example.com", Some(("stun.example.com", 3478))),
            ("stuns:stun.example.com", Some(("stun.example.com", 5349))),
            // Neither a scheme this client probes...
            ("http:example.com", None),
            // ...nor an unparsable entry may cost the session its real servers.
            ("nonsense", None),
            ("", None),
        ];
        for (raw, expected) in cases {
            let servers = [IceServer {
                urls: vec![raw.to_string()],
                username: None,
                credential: None,
            }];
            let endpoints = ice_server_endpoints(&servers);
            match expected {
                Some((host, port)) => assert_eq!(
                    endpoints,
                    vec![(host.to_string(), port)],
                    "endpoint for {raw:?}"
                ),
                None => assert!(
                    endpoints.is_empty(),
                    "{raw:?} must contribute no endpoint, got {endpoints:?}"
                ),
            }
        }

        // Every URL of every server is probed, deduplicated across entries.
        let repeated = [
            IceServer {
                urls: vec![
                    "stun:stun.example.com:3478".to_string(),
                    "turn:turn.example.com:3478?transport=udp".to_string(),
                ],
                username: None,
                credential: None,
            },
            IceServer {
                urls: vec!["stun:stun.example.com:3478".to_string()],
                username: None,
                credential: None,
            },
        ];
        assert_eq!(
            ice_server_endpoints(&repeated),
            vec![
                ("stun.example.com".to_string(), 3478),
                ("turn.example.com".to_string(), 3478),
            ]
        );
    }

    #[test]
    fn a_routing_answer_can_never_defeat_the_requested_ip_family() {
        let loopback_v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let loopback_v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 0));
        for (family, enumerated, rejected_source) in [
            (
                IpFamily::Ipv4,
                loopback_v4,
                IpAddr::from(Ipv6Addr::LOCALHOST),
            ),
            (
                IpFamily::Ipv6,
                loopback_v6,
                IpAddr::from(Ipv4Addr::LOCALHOST),
            ),
        ] {
            let settings = EngineSettings {
                ip_family: family,
                ..EngineSettings::default()
            };
            assert_eq!(
                session_udp_addrs(
                    settings,
                    |_settings| Ok(vec![enumerated]),
                    [rejected_source]
                )
                .expect("the pinned family still resolves"),
                vec![enumerated],
                "--ip-family {} must reject the {rejected_source} routing answer",
                family.as_str()
            );
        }

        // Addresses no peer could dial are dropped from a routing answer for
        // the same reasons they are dropped from the interface table.
        let undialable = [
            IpAddr::from(Ipv4Addr::UNSPECIFIED),
            IpAddr::from(Ipv6Addr::UNSPECIFIED),
            IpAddr::from([224, 0, 0, 1]),
            IpAddr::from([0xfe80, 0, 0, 0, 0, 0, 0, 1]),
        ];
        assert_eq!(
            session_udp_addrs(
                EngineSettings::default(),
                |_settings| Ok(vec![loopback_v4]),
                undialable,
            )
            .expect("undialable answers are filtered, not fatal"),
            vec![loopback_v4]
        );
    }

    #[test]
    fn the_crippled_transport_never_gains_a_routable_source() {
        // `--cripple-ice` exists to be unreachable. Applying the real interface
        // rule proves the guard is the crippled flag, not a fixture.
        assert_eq!(
            session_udp_addrs(
                crippled_settings(IpFamily::Any),
                local_udp_addrs,
                [
                    IpAddr::from(Ipv6Addr::LOCALHOST),
                    IpAddr::from([203, 0, 113, 1])
                ],
            )
            .expect("the crippled transport resolves"),
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))]
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
                gathered: GatheredCandidate {
                    candidate_type: "host".to_string(),
                    address: Ipv4Addr::LOCALHOST.to_string(),
                    port: 1,
                    protocol: "udp".to_string(),
                },
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
                queue_permit: crate::engine::MessageQueuePermit::for_test(),
                received_at: tokio::time::Instant::now(),
            },
            EngineEvent::ChannelReceiveRejected {
                peer,
                generation: stale_generation,
                label: RELIABLE_LABEL.to_string(),
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

    #[tokio::test]
    async fn channel_open_requires_both_current_channels_to_be_open() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(crippled_settings(IpFamily::Ipv4), tx).expect("engine builds");
        let peer = PlayerId::from_u128(0xc);
        assert!(!engine.note_channel_open(peer, RELIABLE_LABEL).await);
        for initiate in [false, true] {
            engine.pair_with(peer, initiate, &[]).await.unwrap();
            // The responder has no channels; initiator channels are connecting.
            // Neither may form a live pair from historical callbacks alone.
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                assert!(!engine.note_channel_open(peer, label).await);
            }
            assert_eq!(engine.connected_pair_count(), 0);
            engine.remove_peer(peer).await.unwrap();
        }
    }

    // Regression #869: remote announcements must not displace local or remote labels.
    #[tokio::test]
    async fn duplicate_remote_channels_preserve_first_channel() {
        for initiate in [false, true] {
            let (tx, _rx) = mpsc::unbounded_channel();
            let mut engine = Engine::new(crippled_settings(IpFamily::Ipv4), tx).unwrap();
            let peer = PlayerId::from_u128(0x869);
            engine.pair_with(peer, initiate, &[]).await.unwrap();
            let pc = engine.peers[&peer].pc.clone();
            for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                let first = match engine.channel(peer, label) {
                    Some(channel) => channel,
                    None => {
                        let channel = pc
                            .create_data_channel(
                                label,
                                Some(RTCDataChannelInit {
                                    ordered: label == RELIABLE_LABEL,
                                    max_retransmits: (label == UNRELIABLE_LABEL).then_some(0),
                                    ..RTCDataChannelInit::default()
                                }),
                            )
                            .await
                            .unwrap();
                        engine
                            .store_remote_channel(peer, label.to_string(), channel.clone())
                            .await;
                        channel
                    }
                };
                engine
                    .store_remote_channel(peer, label.to_string(), first.clone())
                    .await;
                assert_eq!(
                    first.ready_state().await.unwrap(),
                    rtc::data_channel::RTCDataChannelState::Connecting,
                    "repeated original announcement must keep it alive"
                );
                let duplicate = pc
                    .create_data_channel(
                        label,
                        Some(RTCDataChannelInit {
                            ordered: label == RELIABLE_LABEL,
                            max_retransmits: (label == UNRELIABLE_LABEL).then_some(0),
                            ..RTCDataChannelInit::default()
                        }),
                    )
                    .await
                    .unwrap();
                engine
                    .store_remote_channel(peer, label.to_string(), duplicate.clone())
                    .await;
                assert!(
                    Arc::ptr_eq(&first, &engine.channel(peer, label).unwrap()),
                    "initiate={initiate}: preserve first {label}"
                );
                assert!(
                    !matches!(
                        duplicate.ready_state().await,
                        Ok(rtc::data_channel::RTCDataChannelState::Connecting)
                    ),
                    "duplicate must close"
                );
                first.close().await.unwrap();
                let after_close = pc
                    .create_data_channel(
                        label,
                        Some(RTCDataChannelInit {
                            ordered: label == RELIABLE_LABEL,
                            max_retransmits: (label == UNRELIABLE_LABEL).then_some(0),
                            ..RTCDataChannelInit::default()
                        }),
                    )
                    .await
                    .unwrap();
                engine
                    .store_remote_channel(peer, label.to_string(), after_close)
                    .await;
                assert!(
                    Arc::ptr_eq(&first, &engine.channel(peer, label).unwrap()),
                    "closed original keeps its label reserved"
                );
            }
            engine.remove_peer(peer).await.unwrap();
            engine.pair_with(peer, false, &[]).await.unwrap();
            let replacement = engine.peers[&peer]
                .pc
                .create_data_channel(RELIABLE_LABEL, None)
                .await
                .unwrap();
            engine
                .store_remote_channel(peer, RELIABLE_LABEL.to_string(), replacement.clone())
                .await;
            assert!(
                Arc::ptr_eq(&replacement, &engine.channel(peer, RELIABLE_LABEL).unwrap()),
                "fresh physical link accepts the same label"
            );
            engine.remove_peer(peer).await.unwrap();
        }
    }

    // Regression #870: unsupported labels must not reach the queue or channel map.
    #[tokio::test]
    async fn unsupported_remote_channels_close_before_queue_and_storage() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(crippled_settings(IpFamily::Ipv4), tx.clone()).unwrap();
        let peer = PlayerId::from_u128(0x870);
        engine.pair_with(peer, false, &[]).await.unwrap();
        let handler = PeerHandler {
            peer,
            generation: engine.peers[&peer].generation,
            crippled: true,
            events: tx,
            channel_admission: engine.peers[&peer].channel_admission.clone(),
            receive_admission: engine.peers[&peer].receive_admission.clone(),
        };
        let pc = engine.peers[&peer].pc.clone();
        let labels = [
            String::new(),
            "Reliable".into(),
            "reliable\0".into(),
            "界".repeat(4096),
        ]
        .into_iter()
        .chain((0..128).map(|index| format!("optional-{index}")));
        for label in labels {
            let channel = pc.create_data_channel(&label, None).await.unwrap();
            handler.on_data_channel(channel.clone()).await;
            assert!(
                rx.is_empty(),
                "unsupported label must not enter engine event queue"
            );
            assert!(
                matches!(
                    channel.ready_state().await,
                    Ok(rtc::data_channel::RTCDataChannelState::Closing
                        | rtc::data_channel::RTCDataChannelState::Closed)
                ),
                "unsupported channel must close before queuing"
            );
            let direct = pc.create_data_channel(&label, None).await.unwrap();
            engine
                .store_remote_channel(peer, label.clone(), direct.clone())
                .await;
            assert!(
                engine.channel(peer, &label).is_none(),
                "unsupported label retained"
            );
            assert!(engine.peers[&peer].channels.is_empty());
            assert!(matches!(
                direct.ready_state().await,
                Ok(rtc::data_channel::RTCDataChannelState::Closing
                    | rtc::data_channel::RTCDataChannelState::Closed)
            ));
        }
        for label in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
            let channel = pc
                .create_data_channel(
                    label,
                    Some(RTCDataChannelInit {
                        ordered: label == RELIABLE_LABEL,
                        max_retransmits: (label == UNRELIABLE_LABEL).then_some(0),
                        ..RTCDataChannelInit::default()
                    }),
                )
                .await
                .unwrap();
            handler.on_data_channel(channel.clone()).await;
            let EngineEvent::RemoteChannel {
                label: announced,
                channel: accepted,
                ..
            } = rx.recv().await.unwrap()
            else {
                panic!("required announcement missing")
            };
            assert_eq!(announced, label);
            assert!(Arc::ptr_eq(&channel, &accepted));
            engine.store_remote_channel(peer, announced, accepted).await;
        }
        assert_eq!(engine.peers[&peer].channels.len(), 2);
        let extra = pc
            .create_data_channel("after-required", None)
            .await
            .unwrap();
        handler.on_data_channel(extra).await;
        assert!(rx.is_empty());
        assert_eq!(engine.peers[&peer].channels.len(), 2);
        engine.remove_peer(peer).await.unwrap();
        assert!(engine.channel(peer, RELIABLE_LABEL).is_none());
        engine.pair_with(peer, false, &[]).await.unwrap();
        assert!(engine.peers[&peer].channels.is_empty());
        engine.remove_peer(peer).await.unwrap();
    }

    // Regression #869: rejected live channels must never publish pair callbacks.
    #[tokio::test]
    async fn live_duplicate_channels_do_not_publish_callbacks() {
        live_refused_channel_history(false).await;
    }

    // Regression #872: every close is followed by a new open on the same link.
    #[tokio::test]
    async fn live_refused_channels_reopen_and_recover_on_fresh_link() {
        live_refused_channel_history(true).await;
    }

    async fn live_refused_channel_history(sequential: bool) {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
        let (a_tx, mut a_rx) = mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = mpsc::unbounded_channel();
        let mut a = Engine::new(mdns_disabled_settings(), a_tx).expect("engine A builds");
        let mut b = Engine::new(mdns_disabled_settings(), b_tx).expect("engine B builds");
        let a_id = PlayerId::from_u128(0xa);
        let b_id = PlayerId::from_u128(0xb);

        for _ in 0..if sequential { 2 } else { 1 } {
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
                                a.store_remote_channel(b_id, label, channel).await;
                            }
                            EngineEvent::ChannelOpen { label, .. } => {
                                a_connected |= a.note_channel_open(b_id, &label).await;
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
                                b.store_remote_channel(a_id, label, channel).await;
                            }
                            EngineEvent::ChannelOpen { label, .. } => {
                                b_connected |= b.note_channel_open(a_id, &label).await;
                            }
                            _ => {}
                        },
                    }
                }
                assert!(a_pc_connected, "A peer connection must reach Connected");
            })
            .await
            .expect("local peer pair connects");

            for from_initiator in [true, false] {
                let (sender, receiver, events, sender_peer, receiver_peer) = if from_initiator {
                    (&mut a, &mut b, &mut b_rx, b_id, a_id)
                } else {
                    (&mut b, &mut a, &mut a_rx, a_id, b_id)
                };
                let baseline_stats = receiver.peers[&receiver_peer]
                    .pc
                    .get_stats(std::time::Instant::now(), StatsSelector::None)
                    .await;
                let opened_before = baseline_stats
                    .iter()
                    .find_map(|entry| match entry {
                        RTCStatsReportEntry::PeerConnection(stats) => {
                            Some(stats.data_channels_opened)
                        }
                        _ => None,
                    })
                    .expect("peer lifetime statistics");
                let prior_stats: BTreeSet<_> = baseline_stats
                    .iter()
                    .filter_map(|entry| match entry {
                        RTCStatsReportEntry::DataChannel(channel) => Some(channel.stats.id.clone()),
                        _ => None,
                    })
                    .collect();
                // The admission control offers distinct streams as one workload.
                // The reset control waits for each refusal before creating its successor.
                let mut optional_streams = Vec::new();
                let optional_count = if sequential { 82 } else { 18 };
                let mut offered_ids = Vec::new();
                let mut extras = Vec::new();
                for label in [String::new(), "界".repeat(4096)]
                    .into_iter()
                    .chain((0..optional_count - 2).map(|index| format!("optional-{index}")))
                {
                    let channel = if sequential {
                        None
                    } else {
                        let channel = sender.peers[&sender_peer]
                            .pc
                            .create_data_channel(&label, None)
                            .await
                            .unwrap();
                        offered_ids.push(channel.id());
                        Some(channel)
                    };
                    extras.push((label, channel));
                }
                let mut duplicates = Vec::new();
                for label in (0..if sequential { 4 } else { 1 })
                    .flat_map(|_| [RELIABLE_LABEL, UNRELIABLE_LABEL])
                {
                    let init = RTCDataChannelInit {
                        ordered: label == RELIABLE_LABEL,
                        max_retransmits: (label == UNRELIABLE_LABEL).then_some(0),
                        ..RTCDataChannelInit::default()
                    };
                    let channel = if sequential {
                        None
                    } else {
                        let channel = sender.peers[&sender_peer]
                            .pc
                            .create_data_channel(label, Some(init.clone()))
                            .await
                            .unwrap();
                        offered_ids.push(channel.id());
                        Some(channel)
                    };
                    duplicates.push((label, init, channel));
                }
                for (label, extra) in extras {
                    let extra = match extra {
                        Some(extra) => extra,
                        None => {
                            let channel = sender.peers[&sender_peer]
                                .pc
                                .create_data_channel(&label, None)
                                .await
                                .unwrap();
                            offered_ids.push(channel.id());
                            channel
                        }
                    };
                    tokio::time::timeout(Duration::from_secs(5), async {
                        loop {
                            let event = extra
                                .poll()
                                .await
                                .expect("optional channel emits close before ending");
                            if matches!(event, DataChannelEvent::OnClose) {
                                break;
                            }
                        }
                    })
                    .await
                    .expect("remote refusal closes unsupported channel");
                    assert!(receiver.channel(receiver_peer, &label).is_none());
                    if sequential {
                        // Inspect each close before its summary can age out. This
                        // observes every wire OPEN without requiring eternal stats.
                        let stats = tokio::time::timeout(Duration::from_secs(5), async {
                            loop {
                                let stats = receiver.peers[&receiver_peer].pc
                                    .get_stats(std::time::Instant::now(), StatsSelector::None).await;
                                if stats.iter().any(|entry| matches!(entry,
                                    RTCStatsReportEntry::DataChannel(channel)
                                        if channel.label == label && !prior_stats.contains(&channel.stats.id)
                                            && channel.state == rtc::data_channel::RTCDataChannelState::Closed)) {
                                    break stats;
                                }
                                tokio::time::sleep(Duration::from_millis(1)).await;
                            }
                        }).await.expect("receiver completes optional stream reset");
                        let opened = stats
                            .iter()
                            .find_map(|entry| match entry {
                                RTCStatsReportEntry::PeerConnection(stats) => {
                                    Some(stats.data_channels_opened)
                                }
                                _ => None,
                            })
                            .expect("peer lifetime statistics");
                        assert_eq!(
                            opened,
                            opened_before + optional_streams.len() as u32 + 1,
                            "lifetime count preserves every offered OPEN across history eviction"
                        );
                        let channels: Vec<_> = stats
                            .iter()
                            .filter_map(|entry| match entry {
                                RTCStatsReportEntry::DataChannel(channel) => Some(channel),
                                _ => None,
                            })
                            .collect();
                        assert!(
                            channels.len() <= 66,
                            "closed history grows with refused channels: {}",
                            channels.len()
                        );
                        let closed = channels
                            .iter()
                            .find(|channel| {
                                channel.label == label && !prior_stats.contains(&channel.stats.id)
                            })
                            .expect("receiver observes this optional OPEN before history eviction");
                        assert_eq!(closed.state, rtc::data_channel::RTCDataChannelState::Closed);
                        optional_streams.push(closed.data_channel_identifier);
                        for required in [RELIABLE_LABEL, UNRELIABLE_LABEL] {
                            let live = channels.iter().find(|channel| channel.label == required
                                && channel.state == rtc::data_channel::RTCDataChannelState::Open)
                                .expect("live required-channel statistics survive closed-history eviction");
                            assert!(prior_stats.contains(&live.stats.id));
                        }
                    }
                }
                sender
                    .channel(sender_peer, RELIABLE_LABEL)
                    .unwrap()
                    .send_text("required-after-unsupported")
                    .await
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        match events.recv().await.unwrap() {
                            EngineEvent::ChannelMessage { label, text, .. } => {
                                assert_eq!(label, RELIABLE_LABEL);
                                assert_eq!(text, "required-after-unsupported");
                                break;
                            }
                            EngineEvent::RemoteChannel { .. } => {
                                panic!("refused channel announcement entered application queue")
                            }
                            EngineEvent::ChannelOpen { .. } | EngineEvent::ChannelClosed { .. } => {
                                panic!("unsupported channel reached application queue")
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .expect("required traffic survives unsupported channel burst");
                assert_eq!(receiver.peers[&receiver_peer].channels.len(), 2);
                for (label, init, duplicate) in duplicates {
                    let duplicate = match duplicate {
                        Some(duplicate) => duplicate,
                        None => {
                            let channel = sender.peers[&sender_peer]
                                .pc
                                .create_data_channel(label, Some(init))
                                .await
                                .unwrap();
                            offered_ids.push(channel.id());
                            channel
                        }
                    };
                    let original = receiver.channel(receiver_peer, label).unwrap();
                    tokio::time::timeout(Duration::from_secs(5), async {
                    // Callback admission closes duplicates without an orchestrator
                    // event. The sender's close is the remote refusal barrier.
                    loop {
                        let event = duplicate.poll().await.expect("duplicate emits close");
                        if matches!(event, DataChannelEvent::OnClose) {
                            break;
                        }
                    }
                    assert!(
                        Arc::ptr_eq(&original, &receiver.channel(receiver_peer, label).unwrap()),
                        "duplicate must not replace selected live channel"
                    );
                    sender
                        .channel(sender_peer, label)
                        .unwrap()
                        .send_text("original-after-duplicate")
                        .await
                        .unwrap();
                    loop {
                        match events.recv().await.unwrap() {
                            EngineEvent::ChannelMessage {
                                label: received,
                                text,
                                ..
                            } => {
                                assert_eq!(received, label);
                                assert_eq!(text, "original-after-duplicate");
                                break;
                            }
                            EngineEvent::RemoteChannel { .. } => {
                                panic!("duplicate announcement entered application queue")
                            }
                            EngineEvent::ChannelOpen { .. } | EngineEvent::ChannelClosed { .. } => {
                                panic!("duplicate emitted a channel lifecycle callback")
                            }
                            _ => {}
                        }
                    }
                })
                .await
                .unwrap_or_else(|error| panic!("duplicate history timed out: from_initiator={from_initiator}, label={label}: {error}"));
                    // No callbacks from the rejected channel remain after its close.
                    loop {
                        match events.try_recv() {
                            Ok(event) => assert!(
                                !matches!(
                                    event,
                                    EngineEvent::RemoteChannel { .. }
                                        | EngineEvent::ChannelOpen { .. }
                                        | EngineEvent::ChannelClosed { .. }
                                        | EngineEvent::ChannelMessage { .. }
                                ),
                                "rejected channel must not publish callbacks"
                            ),
                            Err(mpsc::error::TryRecvError::Empty) => break,
                            Err(mpsc::error::TryRecvError::Disconnected) => {
                                panic!("live engine event stream must stay connected")
                            }
                        }
                    }
                    assert_eq!(receiver.connected_pair_count(), 1);
                }
                if sequential {
                    // Each wire OPEN was observed at its own close boundary.
                    assert_eq!(optional_streams.len(), optional_count);
                    assert!(
                        optional_streams
                            .iter()
                            .copied()
                            .collect::<BTreeSet<_>>()
                            .len()
                            < optional_count,
                        "reset history must actually reuse wire streams"
                    );
                } else {
                    let stats = sender.peers[&sender_peer]
                        .pc
                        .get_stats(std::time::Instant::now(), StatsSelector::None)
                        .await;
                    let mut offered_streams = BTreeSet::new();
                    for id in offered_ids {
                        let stats_id = format!("RTCDataChannel_{id}");
                        let stream = stats
                            .iter()
                            .find_map(|entry| match entry {
                                RTCStatsReportEntry::DataChannel(channel)
                                    if channel.stats.id == stats_id =>
                                {
                                    Some(channel.data_channel_identifier)
                                }
                                _ => None,
                            })
                            .expect("offered channel has assigned SCTP stream stats");
                        assert!(
                            offered_streams.insert(stream),
                            "admission workload must use distinct SCTP streams"
                        );
                    }
                    assert_eq!(offered_streams.len(), 20);
                }
            }
            a.channel(b_id, RELIABLE_LABEL)
                .unwrap()
                .close()
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let EngineEvent::ChannelClosed { label, .. } = a_rx.recv().await.unwrap() {
                        assert_eq!(label, RELIABLE_LABEL);
                        break;
                    }
                }
            })
            .await
            .expect("selected channel keeps its close callback");
            let a_generation = a.peers[&b_id].generation;
            let b_generation = b.peers[&a_id].generation;
            a.remove_peer(b_id).await.unwrap();
            b.remove_peer(a_id).await.unwrap();
            assert_eq!(a.connected_pair_count(), 0);
            assert_eq!(b.connected_pair_count(), 0);
            // Verify each retired-link callback before starting fresh-link barriers.
            for (events, peer, generation) in [
                (&mut a_rx, b_id, a_generation),
                (&mut b_rx, a_id, b_generation),
            ] {
                loop {
                    match events.try_recv() {
                        Ok(event) => {
                            assert_eq!(event.peer_generation(), (peer, generation));
                            match event {
                                EngineEvent::ChannelClosed { label, .. } => assert!(
                                    matches!(label.as_str(), RELIABLE_LABEL | UNRELIABLE_LABEL),
                                    "only selected required channels may close during teardown"
                                ),
                                EngineEvent::PcState { state, .. } => {
                                    assert_eq!(state, RTCPeerConnectionState::Closed);
                                }
                                EngineEvent::LocalCandidate { candidate_json, .. } => {
                                    assert!(!candidate_json.is_empty());
                                }
                                EngineEvent::IceGatheringComplete { .. } => {}
                                _ => panic!(
                                    "teardown must not emit channel admission or game callbacks"
                                ),
                            }
                        }
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            panic!("engine event stream stays connected for fresh pairing")
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn detached_selected_pair_probe_observes_before_live_channel_closes() {
        let (a_tx, mut a_rx) = mpsc::unbounded_channel();
        let (b_tx, mut b_rx) = mpsc::unbounded_channel();
        let (selected_tx, mut selected_rx) = mpsc::unbounded_channel();
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
                            a.store_remote_channel(b_id, label, channel).await;
                        }
                        EngineEvent::ChannelOpen { label, .. } => {
                            a_connected |= a.note_channel_open(b_id, &label).await;
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
                            b.store_remote_channel(a_id, label, channel).await;
                        }
                        EngineEvent::ChannelOpen { label, .. } => {
                            b_connected |= b.note_channel_open(a_id, &label).await;
                        }
                        _ => {}
                    },
                }
            }
            assert!(a_pc_connected, "A peer connection must reach Connected");
        })
        .await
        .expect("local peer pair connects");

        let selected = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                assert!(
                    a.start_selected_candidate_pair_probe(b_id, selected_tx.clone()),
                    "connected physical link remains available for evidence"
                );
                let result = selected_rx.recv().await.expect("probe channel stays open");
                assert_eq!(result.peer, b_id);
                assert!(a.is_current_generation(result.peer, result.generation));
                let outcome = result.selected;
                if let Some(selected) = outcome {
                    break selected;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("connected pair eventually publishes nominated transport evidence");
        assert!(!selected.local_candidate_type.is_empty());
        assert!(!selected.remote_candidate_type.is_empty());

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

#[cfg(test)]
#[path = "channel_admission_tests.rs"]
mod channel_admission_tests;
