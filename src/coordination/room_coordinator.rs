//! Room operation coordination for distributed state management
//!
//! This module provides coordinators for managing room operations (lobby transitions,
//! authority transfers, player ready states) with distributed locking to ensure consistency.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::distributed::DistributedLock;
use crate::protocol::{PlayerId, RoomId};

use super::MessageCoordinator;

/// Trait for room operation coordination
#[async_trait]
pub trait RoomOperationCoordinatorTrait: Send + Sync {
    /// Transition a room to lobby state
    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> Result<bool>;

    /// Coordinate authority transfer between players
    async fn coordinate_authority_transfer(
        &self,
        room_id: &RoomId,
        new_authority: &PlayerId,
    ) -> Result<bool>;

    /// Execute a distributed operation on a room
    async fn execute_distributed_operation(&self, operation: &str, room_id: &RoomId) -> Result<()>;

    /// Handle authority request from a player
    async fn handle_authority_request(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<(bool, Option<String>)>;

    /// Handle player ready state change
    async fn handle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        app_id: Option<Uuid>,
    ) -> Result<()>;

    /// Clear ready players for a room
    async fn clear_ready_players(&self, room_id: &RoomId) -> Result<()>;
}

/// In-memory room operation coordinator
pub struct InMemoryRoomOperationCoordinator {
    coordinator: Arc<dyn MessageCoordinator>,
    distributed_lock: Arc<dyn DistributedLock>,
    database: Arc<dyn crate::database::GameDatabase>,
    /// Track ready players per room for in-memory coordinator
    ready_players: Arc<RwLock<HashMap<RoomId, HashSet<PlayerId>>>>,
}

impl InMemoryRoomOperationCoordinator {
    /// Create a new in-memory room operation coordinator
    pub fn new(
        coordinator: Arc<dyn MessageCoordinator>,
        distributed_lock: Arc<dyn DistributedLock>,
        database: Arc<dyn crate::database::GameDatabase>,
    ) -> Self {
        Self {
            coordinator,
            distributed_lock,
            database,
            ready_players: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl RoomOperationCoordinatorTrait for InMemoryRoomOperationCoordinator {
    async fn transition_room_to_lobby(&self, room_id: &RoomId) -> Result<bool> {
        // For in-memory implementation, just simulate the operation
        let lock_key = format!("room_lobby_transition:{room_id}");
        let _lock_handle = self
            .distributed_lock
            .acquire(&lock_key, Duration::from_secs(10))
            .await?;

        // Simulate success and broadcast lobby state change
        let message = crate::protocol::ServerMessage::LobbyStateChanged {
            lobby_state: crate::protocol::LobbyState::Lobby,
            ready_players: Vec::new(),
            all_ready: false,
        };

        self.coordinator
            .broadcast_to_room(room_id, Arc::new(message))
            .await?;
        tracing::info!(%room_id, "Room transitioned to lobby state (in-memory)");
        Ok(true)
    }

    async fn coordinate_authority_transfer(
        &self,
        room_id: &RoomId,
        new_authority: &PlayerId,
    ) -> Result<bool> {
        // For in-memory implementation, just simulate the operation
        let lock_key = format!("authority_transfer:{room_id}:{new_authority}");
        let _lock_handle = self
            .distributed_lock
            .acquire(&lock_key, Duration::from_secs(5))
            .await?;

        // Simulate successful authority transfer
        let message = crate::protocol::ServerMessage::AuthorityChanged {
            authority_player: Some(*new_authority),
            you_are_authority: false, // Will be customized per client
        };

        self.coordinator
            .broadcast_to_room(room_id, Arc::new(message))
            .await?;
        tracing::info!(%room_id, %new_authority, "Authority transferred (in-memory)");
        Ok(true)
    }

    async fn execute_distributed_operation(&self, operation: &str, room_id: &RoomId) -> Result<()> {
        // For in-memory implementation, just log the operation
        let lock_key = format!("distributed_op:{room_id}:{operation}");
        let _lock_handle = self
            .distributed_lock
            .acquire(&lock_key, Duration::from_secs(5))
            .await?;

        tracing::info!(%room_id, %operation, "Executed distributed operation (in-memory)");
        Ok(())
    }

    async fn handle_authority_request(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        become_authority: bool,
    ) -> Result<(bool, Option<String>)> {
        // For in-memory implementation, use the actual database authority request method
        let lock_key = format!("room_authority:{room_id}");
        let _lock_handle = self
            .distributed_lock
            .acquire(&lock_key, Duration::from_secs(5))
            .await?;

        tracing::info!(%room_id, %player_id, %become_authority, "InMemory: Processing authority request");

        // Use the database's atomic authority request method
        let result = self
            .database
            .request_room_authority(room_id, player_id, become_authority)
            .await;

        match result {
            Ok((granted, reason)) => {
                if granted {
                    // Broadcast authority change to all players
                    let new_authority = if become_authority {
                        Some(*player_id)
                    } else {
                        None
                    };

                    // First send specific authority response to the requesting player
                    tracing::info!(%room_id, %player_id, "Sending AuthorityResponse to player");
                    let response_message = crate::protocol::ServerMessage::AuthorityResponse {
                        granted,
                        reason: reason.clone(),
                        error_code: if granted {
                            None
                        } else {
                            Some(crate::protocol::ErrorCode::AuthorityDenied)
                        },
                    };

                    if let Err(e) = self
                        .coordinator
                        .send_to_player(player_id, Arc::new(response_message))
                        .await
                    {
                        tracing::error!("Failed to send authority response to player: {}", e);
                    }

                    // Instead of broadcasting, we need to send a custom message to each player
                    // to set the you_are_authority flag correctly
                    tracing::info!(%room_id, "Sending customized AuthorityChanged messages");

                    // Get all player IDs in the room from database
                    let room = match self.database.get_room_by_id(room_id).await {
                        Ok(Some(room)) => room,
                        Ok(None) => {
                            tracing::error!(%room_id, "Room not found when handling authority change");
                            return Ok((granted, reason));
                        }
                        Err(e) => {
                            tracing::error!(%room_id, "Failed to get room: {}", e);
                            return Ok((granted, reason));
                        }
                    };

                    // Send a customized message to each player in the room
                    for room_player_id in room.players.keys() {
                        // This player is the authority if they requested authority and it was granted
                        let is_authority = become_authority && room_player_id == player_id;

                        let auth_message = crate::protocol::ServerMessage::AuthorityChanged {
                            authority_player: new_authority,
                            you_are_authority: is_authority,
                        };

                        tracing::info!(
                            %room_id,
                            %room_player_id,
                            %is_authority,
                            "Sending customized AuthorityChanged message"
                        );

                        if let Err(e) = self
                            .coordinator
                            .send_to_player(room_player_id, Arc::new(auth_message))
                            .await
                        {
                            tracing::error!(%room_player_id, "Failed to send authority change: {}", e);
                        }
                    }

                    tracing::info!(%room_id, %player_id, %become_authority, "Authority request granted (in-memory)");
                } else {
                    // Send denial response to the requesting player
                    let response_message = crate::protocol::ServerMessage::AuthorityResponse {
                        granted,
                        reason: reason.clone(),
                        error_code: if granted {
                            None
                        } else {
                            Some(crate::protocol::ErrorCode::AuthorityDenied)
                        },
                    };

                    if let Err(e) = self
                        .coordinator
                        .send_to_player(player_id, Arc::new(response_message))
                        .await
                    {
                        tracing::error!(
                            "Failed to send authority denial response to player: {}",
                            e
                        );
                    }

                    tracing::info!(%room_id, %player_id, %become_authority, ?reason, "Authority request denied (in-memory)");
                }

                Ok((granted, reason))
            }
            Err(e) => {
                tracing::error!(%room_id, %player_id, %become_authority, "Authority request failed: {}", e);

                // Send error response to the requesting player
                let response_message = crate::protocol::ServerMessage::AuthorityResponse {
                    granted: false,
                    reason: Some("Storage error".to_string()),
                    error_code: Some(crate::protocol::ErrorCode::StorageError),
                };

                if let Err(send_err) = self
                    .coordinator
                    .send_to_player(player_id, Arc::new(response_message))
                    .await
                {
                    tracing::error!("Failed to send error response to player: {}", send_err);
                }

                Ok((false, Some("Storage error".to_string())))
            }
        }
    }

    async fn handle_player_ready(
        &self,
        room_id: &RoomId,
        player_id: &PlayerId,
        _app_id: Option<Uuid>,
    ) -> Result<()> {
        // For in-memory implementation, simulate player ready toggle
        let lock_key = format!("room_ready_state:{room_id}");
        let _lock_handle = self
            .distributed_lock
            .acquire(&lock_key, Duration::from_secs(5))
            .await?;

        // Get current room state to check if it has enough players for lobby actions
        let room = match self.database.get_room_by_id(room_id).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return Err(anyhow::anyhow!("Room not found"));
            }
            Err(e) => {
                return Err(anyhow::anyhow!("Failed to get room: {e}"));
            }
        };

        // Check if room has enough players for lobby state - prevent ready actions if room is no longer full
        if !room.should_enter_lobby() && room.lobby_state != crate::protocol::LobbyState::Lobby {
            return Err(anyhow::anyhow!("Player ready failed: room may not be in lobby state. Current state: {:?}, player count: {}/{}", room.lobby_state, room.players.len(), room.max_players));
        }

        // Toggle player ready state in ready_players map
        let mut ready_map = self.ready_players.write().await;
        let room_ready_players = ready_map.entry(*room_id).or_insert_with(HashSet::new);

        let was_ready = room_ready_players.contains(player_id);
        if was_ready {
            room_ready_players.remove(player_id);
        } else {
            room_ready_players.insert(*player_id);
        }

        let ready_players_vec: Vec<PlayerId> = room_ready_players.iter().copied().collect();

        // Get total players in room to check if all are ready
        let room_players = match self.database.get_room_players(room_id).await {
            Ok(players) => players,
            Err(e) => {
                tracing::error!("Failed to get room players: {}", e);
                Vec::new()
            }
        };

        let all_ready = !room_players.is_empty() && ready_players_vec.len() == room_players.len();

        drop(ready_map); // Release write lock

        // Broadcast lobby state change
        let message = crate::protocol::ServerMessage::LobbyStateChanged {
            lobby_state: crate::protocol::LobbyState::Lobby,
            ready_players: ready_players_vec.clone(),
            all_ready,
        };

        self.coordinator
            .broadcast_to_room(room_id, Arc::new(message))
            .await?;

        // If all players are ready, transition to game starting
        if all_ready {
            // Use P2P connection info from players (no relay server support in signal-fish-server)
            let peer_connections: Vec<crate::protocol::PeerConnectionInfo> = room_players
                .into_iter()
                .map(|player| crate::protocol::PeerConnectionInfo {
                    player_id: player.id,
                    player_name: player.name,
                    is_authority: player.is_authority,
                    relay_type: room.relay_type.clone(),
                    connection_info: player.connection_info,
                })
                .collect();

            let game_start_message =
                Arc::new(crate::protocol::ServerMessage::GameStarting { peer_connections });

            self.coordinator
                .broadcast_to_room(room_id, game_start_message)
                .await?;

            // Clear ready players for this room since game is starting
            let mut ready_map = self.ready_players.write().await;
            ready_map.remove(room_id);
        }

        tracing::info!(%room_id, %player_id, ready = !was_ready, "Player ready state toggled (in-memory)");
        Ok(())
    }

    async fn clear_ready_players(&self, room_id: &RoomId) -> Result<()> {
        // Clear ready players from the in-memory coordinator map
        let mut ready_map = self.ready_players.write().await;
        ready_map.remove(room_id);
        tracing::info!(%room_id, "Cleared ready players from coordinator (in-memory)");
        Ok(())
    }
}
