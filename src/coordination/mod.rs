//! Message coordination and room operation management
//!
//! This module provides facilities for coordinating messages and room operations:
//! - Message deduplication (LRU-based cache)
//! - Room operation coordination with distributed locking
//!
//! For signal-fish-server, this is an in-memory-only implementation.

// Public modules
pub mod dedup;
pub mod room_coordinator;

// Re-export public types
pub use dedup::DedupCacheSettings;
pub use room_coordinator::{InMemoryRoomOperationCoordinator, RoomOperationCoordinatorTrait};

// MessageCoordinator trait (defined in server.rs as InMemoryMessageCoordinator)
use crate::protocol::{PlayerId, RoomId, ServerMessage};
use std::sync::Arc;

#[async_trait::async_trait]
pub trait MessageCoordinator: Send + Sync {
    async fn send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()>;

    async fn broadcast_to_room(
        &self,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()>;

    async fn broadcast_to_room_except(
        &self,
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> anyhow::Result<()>;

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        sender: tokio::sync::mpsc::Sender<Arc<ServerMessage>>,
    ) -> anyhow::Result<()>;

    async fn unregister_local_client(&self, player_id: &PlayerId) -> anyhow::Result<()>;

    async fn should_process_message(
        &self,
        message: &crate::distributed::SequencedMessage,
    ) -> anyhow::Result<bool>;

    async fn mark_message_processed(
        &self,
        message: &crate::distributed::SequencedMessage,
    ) -> anyhow::Result<()>;

    async fn handle_bus_message(
        &self,
        message: crate::distributed::SequencedMessage,
    ) -> anyhow::Result<()>;

    async fn handle_membership_update(
        &self,
        update: crate::coordination::MembershipUpdate,
    ) -> anyhow::Result<()> {
        let _ = update;
        Ok(())
    }
}

/// Membership update for cross-instance coordination.
#[derive(Debug, Clone, serde::Deserialize)]
#[allow(dead_code)]
pub struct MembershipUpdate {
    #[allow(dead_code)]
    pub instance_id: String,
}
