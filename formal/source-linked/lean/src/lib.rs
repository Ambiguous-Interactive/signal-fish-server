#![allow(dead_code)]

#[path = "../../../../src/protocol_core.rs"]
mod protocol_core;

#[path = "../../../../src/session_capability.rs"]
mod session_capability;

// Invoke the production trait adapter so extraction follows its actual implementation.
fn negotiated_profile_capabilities(
    protocol: &session_capability::NegotiatedProtocol,
) -> session_capability::SessionCapabilities {
    session_capability::SessionCapabilities::from(protocol)
}

#[path = "../../../../src/session_admission.rs"]
mod session_admission;
