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

## Related Resources

- [Architecture Documentation](../architecture.md) - Overall system architecture
- [Protocol Reference](../protocol.md) - WebSocket protocol documentation
- [Development Guide](../development.md) - Building and testing
