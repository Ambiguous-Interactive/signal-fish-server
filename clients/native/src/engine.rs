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
//!   `sdpMLineIndex`, matchbox-compatible). Remote candidates that arrive
//!   before the remote description are buffered and flushed afterwards.
//! - **Crippled mode** (`--cripple-ice`): the interface filter rejects every
//!   interface so no host candidates are gathered, and candidate signals are
//!   dropped in both directions — deterministic non-connectivity for fallback
//!   scenarios.
//!
//! The engine is owned and driven by the single orchestrator task. webrtc-rs
//! callbacks never touch engine state directly; they forward through an
//! unbounded [`EngineEvent`] channel back to the orchestrator, which avoids
//! both lock contention and the documented hazards of calling peer-connection
//! methods from inside its own callbacks.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use signal_fish_server::protocol::{IceServer, PlayerId};
use tokio::sync::mpsc;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::setting_engine::SettingEngine;
use webrtc::api::{APIBuilder, API};
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

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
        channel: Arc<RTCDataChannel>,
    },
    /// A data channel (local or remote) reached the open state.
    ChannelOpen {
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
            | Self::PcState {
                peer, generation, ..
            }
            | Self::RemoteChannel {
                peer, generation, ..
            }
            | Self::ChannelOpen {
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
    pc: Arc<RTCPeerConnection>,
    /// Channels by label, both locally created (initiator) and remotely
    /// announced (responder).
    channels: HashMap<String, Arc<RTCDataChannel>>,
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
    api: API,
    crippled: bool,
    events: mpsc::UnboundedSender<EngineEvent>,
    peers: HashMap<PlayerId, PeerLink>,
    next_generation: u64,
}

impl Engine {
    /// Build the engine: default `MediaEngine` codecs + default interceptor
    /// registry; the `SettingEngine` is default unless `crippled`, in which
    /// case the ICE interface filter rejects every interface.
    pub fn new(crippled: bool, events: mpsc::UnboundedSender<EngineEvent>) -> Result<Self> {
        let mut media_engine = MediaEngine::default();
        media_engine
            .register_default_codecs()
            .context("register default codecs")?;
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)
            .context("register default interceptors")?;
        let mut setting_engine = SettingEngine::default();
        if crippled {
            // Reject every interface: no host candidates are ever gathered.
            setting_engine.set_interface_filter(Box::new(|_interface: &str| false));
        }
        let api = APIBuilder::new()
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_setting_engine(setting_engine)
            .build();
        Ok(Self {
            api,
            crippled,
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

        let config = RTCConfiguration {
            ice_servers: convert_ice_servers(ice_servers),
            ..RTCConfiguration::default()
        };
        let pc = Arc::new(
            self.api
                .new_peer_connection(config)
                .await
                .context("create RTCPeerConnection")?,
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
        self.register_pc_handlers(peer, generation, &pc);

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
                        ordered: Some(false),
                        max_retransmits: Some(0),
                        ..RTCDataChannelInit::default()
                    }),
                )
                .await
                .context("create unreliable data channel")?;
            for channel in [&reliable, &unreliable] {
                self.register_channel_handlers(peer, generation, channel);
            }
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
    pub fn store_remote_channel(&mut self, peer: PlayerId, channel: Arc<RTCDataChannel>) {
        if let Some(link) = self.peers.get_mut(&peer) {
            link.channels.insert(channel.label().to_string(), channel);
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
    pub fn channel(&self, peer: PlayerId, label: &str) -> Option<Arc<RTCDataChannel>> {
        self.peers
            .get(&peer)
            .and_then(|link| link.channels.get(label))
            .cloned()
    }

    /// Wire the peer-connection level callbacks for `peer`.
    ///
    /// Callbacks only forward through the event channel (sends to a dropped
    /// receiver are ignored: that happens only during shutdown).
    fn register_pc_handlers(&self, peer: PlayerId, generation: u64, pc: &Arc<RTCPeerConnection>) {
        let events = self.events.clone();
        pc.on_peer_connection_state_change(Box::new(move |state: RTCPeerConnectionState| {
            let _ = events.send(EngineEvent::PcState {
                peer,
                generation,
                state,
            });
            Box::pin(async {})
        }));

        let events = self.events.clone();
        let crippled = self.crippled;
        pc.on_ice_candidate(Box::new(move |candidate| {
            // `None` marks end-of-gathering; crippled mode drops everything.
            if crippled {
                return Box::pin(async {});
            }
            let Some(candidate) = candidate else {
                return Box::pin(async {});
            };
            match candidate
                .to_json()
                .map_err(anyhow::Error::from)
                .and_then(|init| serde_json::to_string(&init).map_err(anyhow::Error::from))
            {
                Ok(candidate_json) => {
                    let _ = events.send(EngineEvent::LocalCandidate {
                        peer,
                        generation,
                        candidate_json,
                    });
                }
                Err(error) => {
                    tracing::warn!(%peer, %error, "failed to serialize local ICE candidate");
                }
            }
            Box::pin(async {})
        }));

        let events = self.events.clone();
        pc.on_data_channel(Box::new(move |channel: Arc<RTCDataChannel>| {
            // Hand the channel to the orchestrator FIRST so its bookkeeping
            // exists before any open/message notification for it.
            let _ = events.send(EngineEvent::RemoteChannel {
                peer,
                generation,
                channel: channel.clone(),
            });
            register_channel_handlers_on(&events, peer, generation, &channel);
            Box::pin(async {})
        }));
    }

    /// Wire open/message callbacks on a locally created channel.
    fn register_channel_handlers(
        &self,
        peer: PlayerId,
        generation: u64,
        channel: &Arc<RTCDataChannel>,
    ) {
        register_channel_handlers_on(&self.events, peer, generation, channel);
    }
}

/// Wire `on_open` / `on_message` so both forward to the orchestrator.
///
/// webrtc-rs invokes an `on_open` handler immediately when the channel is
/// already open at registration time, so the responder path (registration
/// inside `on_data_channel`) cannot miss the open edge; `OnOpenHdlrFn` is
/// `FnOnce`, so the open event fires at most once per channel.
fn register_channel_handlers_on(
    events: &mpsc::UnboundedSender<EngineEvent>,
    peer: PlayerId,
    generation: u64,
    channel: &Arc<RTCDataChannel>,
) {
    let label = channel.label().to_string();

    let open_events = events.clone();
    let open_label = label.clone();
    channel.on_open(Box::new(move || {
        let _ = open_events.send(EngineEvent::ChannelOpen {
            peer,
            generation,
            label: open_label,
        });
        Box::pin(async {})
    }));

    let message_events = events.clone();
    channel.on_message(Box::new(move |message| {
        let text = String::from_utf8_lossy(&message.data).into_owned();
        let _ = message_events.send(EngineEvent::ChannelMessage {
            peer,
            generation,
            label: label.clone(),
            text,
        });
        Box::pin(async {})
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

    #[tokio::test]
    async fn pairing_is_idempotent_and_initiator_offers() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut engine = Engine::new(false, tx).expect("engine builds");
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
            EngineEvent::PcState {
                peer,
                generation: stale_generation,
                state: RTCPeerConnectionState::Connected,
            },
            EngineEvent::RemoteChannel {
                peer,
                generation: stale_generation,
                channel: stale_channel,
            },
            EngineEvent::ChannelOpen {
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
        let mut engine = Engine::new(false, tx).expect("engine builds");
        let peer = PlayerId::from_u128(0xc);
        // Unknown peer: never connected.
        assert!(!engine.note_channel_open(peer, RELIABLE_LABEL));
    }
}
