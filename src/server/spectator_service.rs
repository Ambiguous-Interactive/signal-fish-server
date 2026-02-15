use std::sync::Arc;

use dashmap::DashMap;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::ProtocolConfig;
use crate::coordination::MessageCoordinator;
use crate::database::GameDatabase;
use crate::protocol::{
    validation, ErrorCode, PlayerId, PlayerInfo, RoomId, ServerMessage, SpectatorInfo,
    SpectatorJoinedPayload, SpectatorStateChangeReason,
};

#[cfg(test)]
use crate::protocol::Room;

pub(crate) struct SpectatorService {
    spectator_rooms: DashMap<PlayerId, RoomId>,
    database: Arc<dyn GameDatabase>,
    message_coordinator: Arc<dyn MessageCoordinator>,
    room_applications: Arc<DashMap<RoomId, Uuid>>,
    protocol_config: ProtocolConfig,
}

#[derive(Debug)]
pub(crate) struct SpectatorError {
    pub message: String,
    pub code: Option<ErrorCode>,
}

impl SpectatorError {
    fn new(message: impl Into<String>, code: Option<ErrorCode>) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }
}

impl SpectatorService {
    pub(crate) fn new(
        database: Arc<dyn GameDatabase>,
        message_coordinator: Arc<dyn MessageCoordinator>,
        room_applications: Arc<DashMap<RoomId, Uuid>>,
        protocol_config: ProtocolConfig,
    ) -> Self {
        Self {
            spectator_rooms: DashMap::new(),
            database,
            message_coordinator,
            room_applications,
            protocol_config,
        }
    }

    pub(crate) async fn join(
        &self,
        player_id: &PlayerId,
        game_name: String,
        room_code: String,
        spectator_name: String,
    ) -> Result<(), SpectatorError> {
        if let Err(err) =
            validation::validate_player_name_with_config(&spectator_name, &self.protocol_config)
        {
            return Err(SpectatorError::new(err, Some(ErrorCode::InvalidPlayerName)));
        }

        let room = match self.database.get_room(&game_name, &room_code).await {
            Ok(Some(room)) => room,
            Ok(None) => {
                return Err(SpectatorError::new(
                    "Room not found",
                    Some(ErrorCode::RoomNotFound),
                ))
            }
            Err(err) => {
                warn!("Failed to fetch room for spectator: {err}");
                return Err(SpectatorError::new(
                    "Storage error",
                    Some(ErrorCode::StorageError),
                ));
            }
        };

        if !room.can_spectate() {
            return Err(SpectatorError::new(
                "Spectator limit reached",
                Some(ErrorCode::TooManySpectators),
            ));
        }

        let spectator = SpectatorInfo {
            id: *player_id,
            name: spectator_name.clone(),
            connected_at: chrono::Utc::now(),
        };

        match self
            .database
            .add_spectator_to_room(&room.id, spectator.clone())
            .await
        {
            Ok(true) => {
                let current_players: Vec<PlayerInfo> = room.players.values().cloned().collect();
                let spectator_snapshot = self
                    .database
                    .get_room_spectators(&room.id)
                    .await
                    .unwrap_or_default();

                if let Some(previous_room) = self.spectator_rooms.insert(*player_id, room.id) {
                    warn!(
                        %player_id,
                        room_id = %previous_room,
                        new_room_id = %room.id,
                        "Spectator was already mapped to a different room; overwriting"
                    );
                }

                let join_reason = SpectatorStateChangeReason::Joined;

                let _ = self
                    .message_coordinator
                    .send_to_player(
                        player_id,
                        Arc::new(ServerMessage::SpectatorJoined(Box::new(
                            SpectatorJoinedPayload {
                                room_id: room.id,
                                room_code: room.code.clone(),
                                spectator_id: *player_id,
                                game_name: room.game_name.clone(),
                                current_players,
                                current_spectators: spectator_snapshot.clone(),
                                lobby_state: room.lobby_state.clone(),
                                reason: Some(join_reason.clone()),
                            },
                        ))),
                    )
                    .await;

                let notification = Arc::new(ServerMessage::NewSpectatorJoined {
                    spectator: spectator.clone(),
                    current_spectators: spectator_snapshot.clone(),
                    reason: Some(join_reason),
                });

                let _ = self
                    .message_coordinator
                    .broadcast_to_room(&room.id, notification)
                    .await;

                info!(
                    %player_id,
                    spectator_name,
                    room_code,
                    "Spectator joined room"
                );

                Ok(())
            }
            Ok(false) => Err(SpectatorError::new(
                "Failed to join as spectator",
                Some(ErrorCode::SpectatorJoinFailed),
            )),
            Err(err) => {
                warn!("Storage error adding spectator: {err}");
                Err(SpectatorError::new(
                    "Storage error",
                    Some(ErrorCode::StorageError),
                ))
            }
        }
    }

    pub(crate) async fn leave(&self, player_id: &PlayerId) -> Result<(), SpectatorError> {
        if self
            .detach(player_id, SpectatorStateChangeReason::VoluntaryLeave)
            .await
        {
            Ok(())
        } else {
            Err(SpectatorError::new(
                "You are not currently spectating a room",
                Some(ErrorCode::NotASpectator),
            ))
        }
    }

    pub(crate) async fn detach(
        &self,
        player_id: &PlayerId,
        reason: SpectatorStateChangeReason,
    ) -> bool {
        let Some((_, room_id)) = self.spectator_rooms.remove(player_id) else {
            return false;
        };

        if let Err(err) = self
            .database
            .remove_spectator_from_room(&room_id, player_id)
            .await
        {
            warn!(
                %player_id,
                %room_id,
                error = %err,
                "Failed to remove spectator from persistence"
            );
        }

        let current_spectators = match self.database.get_room_spectators(&room_id).await {
            Ok(list) => list,
            Err(err) => {
                warn!(
                    %room_id,
                    error = %err,
                    "Failed to fetch current spectator snapshot"
                );
                Vec::new()
            }
        };

        let room = match self.database.get_room_by_id(&room_id).await {
            Ok(room) => room,
            Err(err) => {
                warn!(
                    %room_id,
                    error = %err,
                    "Failed to fetch room while removing spectator"
                );
                None
            }
        };

        let _ = self
            .message_coordinator
            .send_to_player(
                player_id,
                Arc::new(ServerMessage::SpectatorLeft {
                    room_id: Some(room_id),
                    room_code: room.as_ref().map(|r| r.code.clone()),
                    reason: Some(reason.clone()),
                    current_spectators: current_spectators.clone(),
                }),
            )
            .await;

        if let Some(room) = room {
            let notification = Arc::new(ServerMessage::SpectatorDisconnected {
                spectator_id: *player_id,
                reason: Some(reason),
                current_spectators,
            });

            let _ = self
                .message_coordinator
                .broadcast_to_room(&room.id, notification)
                .await;
        }

        true
    }

    #[allow(dead_code)]
    fn room_app_id(&self, room_id: &RoomId) -> Option<Uuid> {
        self.room_applications
            .get(room_id)
            .map(|entry| *entry.value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::MembershipUpdate;
    use crate::database::{GameDatabase, InMemoryDatabase};
    use crate::distributed::SequencedMessage;
    use anyhow::Result;
    use async_trait::async_trait;
    use tokio::sync::{mpsc, Mutex};

    struct RecordingCoordinator {
        sent: Mutex<Vec<(PlayerId, ServerMessage)>>,
        database: Arc<InMemoryDatabase>,
    }

    impl RecordingCoordinator {
        fn new(database: Arc<InMemoryDatabase>) -> Self {
            Self {
                sent: Mutex::new(Vec::new()),
                database,
            }
        }

        async fn messages_for(&self, player_id: &PlayerId) -> Vec<ServerMessage> {
            self.sent
                .lock()
                .await
                .iter()
                .filter(|(pid, _)| pid == player_id)
                .map(|(_, message)| message.clone())
                .collect()
        }
    }

    #[async_trait]
    impl MessageCoordinator for RecordingCoordinator {
        async fn send_to_player(
            &self,
            player_id: &PlayerId,
            message: Arc<ServerMessage>,
        ) -> Result<()> {
            self.sent
                .lock()
                .await
                .push((*player_id, (*message).clone()));
            Ok(())
        }

        async fn broadcast_to_room(
            &self,
            room_id: &RoomId,
            message: Arc<ServerMessage>,
        ) -> Result<()> {
            if let Ok(Some(room)) = self.database.get_room_by_id(room_id).await {
                let mut sent = self.sent.lock().await;
                for player_id in room.players.keys() {
                    sent.push((*player_id, (*message).clone()));
                }
            }
            Ok(())
        }

        async fn broadcast_to_room_except(
            &self,
            _room_id: &RoomId,
            _except_player: &PlayerId,
            _message: Arc<ServerMessage>,
        ) -> Result<()> {
            Ok(())
        }

        async fn register_local_client(
            &self,
            _player_id: PlayerId,
            _room_id: Option<RoomId>,
            _sender: mpsc::Sender<Arc<ServerMessage>>,
        ) -> Result<()> {
            Ok(())
        }

        async fn unregister_local_client(&self, _player_id: &PlayerId) -> Result<()> {
            Ok(())
        }

        async fn should_process_message(&self, _message: &SequencedMessage) -> Result<bool> {
            Ok(true)
        }

        async fn mark_message_processed(&self, _message: &SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_bus_message(&self, _message: SequencedMessage) -> Result<()> {
            Ok(())
        }

        async fn handle_membership_update(&self, _update: MembershipUpdate) -> Result<()> {
            Ok(())
        }
    }

    async fn setup_service() -> (
        SpectatorService,
        Room,
        PlayerId,
        Arc<RecordingCoordinator>,
        Arc<InMemoryDatabase>,
    ) {
        let database = Arc::new(InMemoryDatabase::new());
        let creator_id = PlayerId::new_v4();
        let room = database
            .create_room(
                "spectator-game".to_string(),
                None,
                8,
                true,
                creator_id,
                "udp".to_string(),
                "region-a".to_string(),
                None,
            )
            .await
            .expect("room creation succeeds");

        let coordinator = Arc::new(RecordingCoordinator::new(database.clone()));
        let spectator_service = SpectatorService::new(
            database.clone() as Arc<dyn GameDatabase>,
            coordinator.clone(),
            Arc::new(DashMap::new()),
            ProtocolConfig::default(),
        );

        (spectator_service, room, creator_id, coordinator, database)
    }

    #[tokio::test]
    async fn join_tracks_room_membership_and_notifies_players() {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();

        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Spectator One".to_string(),
            )
            .await
            .expect("spectator join succeeds");

        assert_eq!(
            service
                .spectator_rooms
                .get(&spectator_id)
                .map(|entry| *entry.value()),
            Some(room.id)
        );

        let stored_spectators = database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch spectators");
        assert!(
            stored_spectators.iter().any(|info| info.id == spectator_id),
            "spectator should be persisted in room snapshot"
        );

        let spectator_messages = coordinator.messages_for(&spectator_id).await;
        assert!(
            spectator_messages.into_iter().any(|message| matches!(
                message,
                ServerMessage::SpectatorJoined(ref payload) if payload.room_id == room.id && payload.spectator_id == spectator_id
            )),
            "spectator should receive SpectatorJoined payload"
        );

        let player_messages = coordinator.messages_for(&creator_id).await;
        assert!(
            player_messages.into_iter().any(|message| matches!(
                message,
                ServerMessage::NewSpectatorJoined { spectator, .. }
                    if spectator.id == spectator_id
            )),
            "room players should see NewSpectatorJoined notification"
        );
    }

    #[tokio::test]
    async fn leave_detaches_spectator_and_sends_disconnect_notifications() {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();

        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Spectator One".to_string(),
            )
            .await
            .expect("spectator join succeeds");

        service.leave(&spectator_id).await.expect("leave succeeds");

        assert!(
            service.spectator_rooms.get(&spectator_id).is_none(),
            "spectator mapping should be cleared after leaving"
        );

        let stored_spectators = database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch spectators after leave");
        assert!(
            stored_spectators.is_empty(),
            "room should no longer track active spectators"
        );

        let spectator_messages = coordinator.messages_for(&spectator_id).await;
        assert!(
            spectator_messages.into_iter().any(|message| matches!(
                message,
                ServerMessage::SpectatorLeft {
                    room_id: Some(left_room),
                    reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
                    ..
                } if left_room == room.id
            )),
            "spectator should receive SpectatorLeft notification with voluntary leave reason"
        );

        let player_messages = coordinator.messages_for(&creator_id).await;
        assert!(
            player_messages.into_iter().any(|message| matches!(
                message,
                ServerMessage::SpectatorDisconnected {
                    spectator_id: sid,
                    reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
                    ..
                } if sid == spectator_id
            )),
            "players should see SpectatorDisconnected with voluntary leave reason"
        );
    }
}
