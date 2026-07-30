# Architecture Decision Records (ADRs)

This directory contains Architecture Decision Records (ADRs) for the Signal Fish Server project.

## What are ADRs?

Architecture Decision Records document important architectural decisions made during the development of the project.
Each ADR captures:

- **Context**: The problem or situation requiring a decision
- **Decision**: The architectural choice that was made
- **Consequences**: The impacts (both positive and negative) of the decision
- **Alternatives**: Other options that were considered and why they were rejected

ADRs are immutable once accepted. If a decision needs to be changed, a new ADR should supersede the old one.

## ADR Index

| ADR | Title | Status |
|-----|-------|--------|
| [ADR-001](reconnection-protocol.md) | Reconnection Protocol | Accepted |
| [ADR-0001](0001-protocol-v3-two-axis.md) | Protocol v3: Two-Axis (Topology + Transport) Capability-Gated Signaling | Accepted |
| [ADR-0002](0002-matchbox-compatibility.md) | Matchbox-Compatible Signal Payloads | Accepted |
| [ADR-0003](0003-formal-verification-and-fuzzing.md) | Formal Verification and Fuzzing for the Protocol v3 Session Core | Accepted |
| [ADR-0004](0004-native-reference-client.md) | Native Reference Client with a Real WebRTC Stack | Accepted |
| [ADR-0005](0005-browser-reference-client.md) | Browser Reference Client on Real Headless Chromium | Accepted |
| [ADR-0006](0006-protocol-v3-delivery-reliability.md) | Protocol v3 Delivery Reliability and Lifecycle Boundaries | Accepted |
| [ADR-0007](0007-bounded-recipient-progress.md) | Bounded Recipient Progress and WebSocket Liveness | Accepted |
| [ADR-0008](0008-single-home-consistency-boundary.md) | Single-Home Consistency and Durability Boundary | Accepted |

> Numbering note: the legacy `ADR-001` (3-digit) predates the current `ADR-0001` 4-digit scheme.
> New ADRs use the 4-digit `ADR-NNNN` form; the legacy ID is preserved as-is to keep existing links stable.

## Related Resources

- [Architecture](../architecture.md) - Overall system architecture
- [Protocol](../protocol.md) - WebSocket protocol documentation
- [Development](../development.md) - Building and testing
