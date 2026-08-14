//! Native Rust reference client for the Signal Fish protocol v3.
//!
//! This crate proves the protocol end to end with a REAL WebRTC stack
//! (`webrtc`-rs): it authenticates against a Signal Fish server, walks the
//! lobby flow to `GameStarting` + `SessionPlan`, establishes one
//! `RTCPeerConnection` per planned peer (two data channels each: `reliable`
//! and `unreliable`), trickles ICE through the server's opaque `Signal`
//! relay, reports the transport-fallback `TransportStatus`, and keeps exercising the
//! WebSocket relay floor (`GameData`) the whole time.
//!
//! It is intentionally a standalone package (not part of the server's cargo
//! package): protocol types are consumed via a path dependency on
//! `signal-fish-server`, so the wire contract cannot drift, while the server
//! crate itself stays free of any WebRTC dependency.
//!
//! # Layout
//!
//! - [`cli`] — scriptable flags (room mode, expected peers, exchange/relay
//!   probes, fault injection, run windows).
//! - [`events`] — the JSONL stdout contract consumed by harnesses.
//! - [`wire`] — WebSocket layer using the server crate's
//!   `ClientMessage`/`ServerMessage` enums verbatim.
//! - [`engine`] — the webrtc-rs integration (pairing, channels, trickle ICE,
//!   crippled-ICE fault mode).
//! - [`client`] — the single-task orchestrator and exit-code policy.

pub mod accountability;
pub mod cli;
pub mod client;
pub mod deadline;
pub mod engine;
pub mod events;
pub mod wire;
