use super::*;

impl SpectatorService {
    #[cfg(test)]
    pub(crate) fn panic_spectator_join_for_test(&self, point: SpectatorJoinPanicPoint) {
        self.join_panic_point
            .store(point as u8, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn panic_spectator_detach_for_test(&self, point: SpectatorDetachPanicPoint) {
        self.detach_panic_point
            .store(point as u8, std::sync::atomic::Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn trigger_spectator_detach_panic_for_test(&self, point: SpectatorDetachPanicPoint) {
        if self
            .detach_panic_point
            .compare_exchange(
                point as u8,
                0,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            panic!("injected spectator detach panic");
        }
    }

    #[cfg(test)]
    pub(super) fn trigger_spectator_join_panic_for_test(&self, point: SpectatorJoinPanicPoint) {
        if self
            .join_panic_point
            .compare_exchange(
                point as u8,
                0,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            panic!("injected spectator join panic");
        }
    }
}
use crate::coordination::{
    MembershipUpdate, RoomEventCompletion, RoomEventJob, RoomEventMutationGuard, RoomEventSequencer,
};
use crate::database::{GameDatabase, InMemoryDatabase};
use crate::distributed::SequencedMessage;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use tokio::sync::{Mutex, Notify, Semaphore};

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum RegistrationFault {
    None,
    PanicBefore,
    PanicAfter,
    AlwaysPanicBefore,
    AlwaysPanicAfter,
    AlwaysError,
}

struct RecordingCoordinator {
    sent: Mutex<Vec<(PlayerId, ServerMessage)>>,
    panic_registration: AtomicU8,
    registration_calls: AtomicUsize,
    deliveries: Mutex<HashMap<PlayerId, crate::coordination::ClientDeliveryHandle>>,
    deliver_registered: AtomicBool,
    database: Arc<InMemoryDatabase>,
    routed_players: Mutex<HashMap<RoomId, HashSet<PlayerId>>>,
    room_events: Arc<RoomEventSequencer>,
    mutation_lock_attempts: AtomicUsize,
    delay_first_spectator_broadcast: AtomicBool,
    spectator_broadcasts: AtomicUsize,
    first_spectator_broadcast_started: Notify,
    release_first_spectator_broadcast: Notify,
    delay_room_closed_sends: AtomicBool,
    room_closed_sends_started: AtomicUsize,
    room_closed_send_started: Notify,
    release_room_closed_sends: Semaphore,
}

impl RecordingCoordinator {
    fn new(database: Arc<InMemoryDatabase>) -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            panic_registration: AtomicU8::new(0),
            registration_calls: AtomicUsize::new(0),
            deliveries: Mutex::new(HashMap::new()),
            deliver_registered: AtomicBool::new(false),
            database,
            routed_players: Mutex::new(HashMap::new()),
            room_events: Arc::new(RoomEventSequencer::default()),
            mutation_lock_attempts: AtomicUsize::new(0),
            delay_first_spectator_broadcast: AtomicBool::new(false),
            spectator_broadcasts: AtomicUsize::new(0),
            first_spectator_broadcast_started: Notify::new(),
            release_first_spectator_broadcast: Notify::new(),
            delay_room_closed_sends: AtomicBool::new(false),
            room_closed_sends_started: AtomicUsize::new(0),
            room_closed_send_started: Notify::new(),
            release_room_closed_sends: Semaphore::new(0),
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

    fn delay_first_spectator_broadcast(&self) {
        self.delay_first_spectator_broadcast
            .store(true, Ordering::Release);
    }
}

#[async_trait]
impl MessageCoordinator for RecordingCoordinator {
    async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard {
        self.mutation_lock_attempts.fetch_add(1, Ordering::AcqRel);
        self.room_events.lock(*room_id).await
    }

    fn enqueue_room_event(
        &self,
        mutation_guard: RoomEventMutationGuard,
        job: RoomEventJob,
    ) -> RoomEventCompletion {
        self.room_events.enqueue(mutation_guard, job)
    }

    async fn send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> Result<()> {
        if matches!(message.as_ref(), ServerMessage::SpectatorLeft { .. })
            && self.delay_room_closed_sends.load(Ordering::Acquire)
        {
            self.room_closed_sends_started
                .fetch_add(1, Ordering::AcqRel);
            self.room_closed_send_started.notify_waiters();
            self.release_room_closed_sends
                .acquire()
                .await
                .expect("test semaphore remains open")
                .forget();
        }
        if self.deliver_registered.load(Ordering::Acquire) {
            if let Some(handle) = self.deliveries.lock().await.get(player_id).cloned() {
                let outcome = handle
                    .sender
                    .try_send(Arc::clone(&message), None)
                    .map_err(|error| anyhow::anyhow!("{error:?}"))?;
                anyhow::ensure!(outcome.enqueued, "registered handle rejected delivery");
            }
        }
        self.sent
            .lock()
            .await
            .push((*player_id, (*message).clone()));
        Ok(())
    }

    async fn try_send_to_player(
        &self,
        player_id: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> Result<bool> {
        // Test double: send_to_player is non-blocking here, so delegating
        // honors the non-waiting farewell contract while preserving
        // whatever recording/blocking behavior the double implements.
        self.send_to_player(player_id, message).await.map(|()| true)
    }

    async fn broadcast_to_room(&self, room_id: &RoomId, message: Arc<ServerMessage>) -> Result<()> {
        if matches!(message.as_ref(), ServerMessage::NewSpectatorJoined { .. })
            && self.delay_first_spectator_broadcast.load(Ordering::Acquire)
            && self.spectator_broadcasts.fetch_add(1, Ordering::AcqRel) == 0
        {
            self.first_spectator_broadcast_started.notify_one();
            self.release_first_spectator_broadcast.notified().await;
        }
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
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
    ) -> Result<()> {
        if let Ok(Some(room)) = self.database.get_room_by_id(room_id).await {
            let mut sent = self.sent.lock().await;
            for player_id in room.players.keys() {
                if player_id != except_player {
                    sent.push((*player_id, (*message).clone()));
                }
            }
        }
        Ok(())
    }

    async fn broadcast_to_room_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        message: Arc<ServerMessage>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> Result<bool> {
        before_send().await;
        self.broadcast_to_room(room_id, message).await?;
        Ok(true)
    }

    async fn broadcast_to_room_if_members_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        expected_members: &[PlayerId],
        message: Arc<ServerMessage>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> Result<bool> {
        let mut routed = self.routed_player_ids(room_id).await?.unwrap_or_default();
        let mut expected = expected_members.to_vec();
        routed.sort_unstable();
        expected.sort_unstable();
        if routed != expected {
            return Ok(false);
        }
        self.broadcast_to_room_with_hook(room_id, message, before_send)
            .await
    }

    async fn broadcast_to_room_except_if_with_hook<'a>(
        &'a self,
        room_id: &RoomId,
        except_player: &PlayerId,
        message: Arc<ServerMessage>,
        should_send: &(dyn Fn() -> bool + Send + Sync),
        drain: tokio::sync::watch::Receiver<bool>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> Result<bool> {
        if *drain.borrow() || !should_send() {
            return Ok(false);
        }
        before_send().await;
        self.broadcast_to_room_except(room_id, except_player, message)
            .await?;
        Ok(true)
    }

    async fn commit_room_messages_if_members_with_hook<'a>(
        &'a self,
        _room_id: &RoomId,
        _expected_members: &[PlayerId],
        recipient_messages: Vec<crate::coordination::RoomRecipientMessages>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>,
                > + Send
                + 'a,
        >,
        after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
    ) -> Result<crate::coordination::RoomMessageTransactionOutcome> {
        if !before_send().await? {
            return Ok(crate::coordination::RoomMessageTransactionOutcome::HookRejected);
        }
        let mut sent = self.sent.lock().await;
        let max_phases = recipient_messages
            .iter()
            .map(crate::coordination::RoomRecipientMessages::phase_count)
            .max()
            .unwrap_or(0);
        let mut after_first_phase = Some(after_first_phase);
        for phase in 0..max_phases {
            for batch in &recipient_messages {
                if let Some(message) = batch.message_in_phase(phase) {
                    sent.push((batch.player_id, message.as_ref().clone()));
                }
            }
            if phase == 0
                && !after_first_phase
                    .take()
                    .expect("transaction state callback runs once")(0)
            {
                break;
            }
        }
        Ok(crate::coordination::RoomMessageTransactionOutcome::Committed)
    }

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        delivery: crate::coordination::ClientDeliveryHandle,
    ) -> Result<()> {
        self.registration_calls.fetch_add(1, Ordering::AcqRel);
        let persistent = self.panic_registration.load(Ordering::Acquire);
        let panic_registration = if persistent >= 3 {
            persistent
        } else {
            self.panic_registration.swap(0, Ordering::AcqRel)
        };
        assert!(
            !matches!(panic_registration, 1 | 3),
            "injected panic before registration"
        );
        if panic_registration == 5 {
            anyhow::bail!("injected registration error");
        }
        self.deliveries.lock().await.insert(player_id, delivery);
        let mut routed_players = self.routed_players.lock().await;
        routed_players.retain(|_, players| {
            players.remove(&player_id);
            !players.is_empty()
        });
        if let Some(room_id) = room_id {
            routed_players.entry(room_id).or_default().insert(player_id);
        }
        drop(routed_players);
        assert!(
            !matches!(panic_registration, 2 | 4),
            "injected panic after registration"
        );
        Ok(())
    }

    async fn unroute_local_client_with_tail<'a>(
        &'a self,
        player_id: PlayerId,
        _room_id: RoomId,
        clear_assignment: Box<
            dyn FnOnce() -> Option<(crate::coordination::ClientDeliveryHandle, u32, u64)>
                + Send
                + 'a,
        >,
    ) -> Result<Option<(u32, u64)>> {
        let Some((_delivery, epoch, final_seq)) = clear_assignment() else {
            return Ok(None);
        };
        let mut routed_players = self.routed_players.lock().await;
        routed_players.retain(|_, players| {
            players.remove(&player_id);
            !players.is_empty()
        });
        Ok(Some((epoch, final_seq)))
    }

    async fn routed_player_ids(&self, room_id: &RoomId) -> Result<Option<Vec<PlayerId>>> {
        Ok(Some(
            self.routed_players
                .lock()
                .await
                .get(room_id)
                .into_iter()
                .flat_map(|players| players.iter().copied())
                .collect(),
        ))
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
    let metrics = Arc::new(crate::metrics::ServerMetrics::new());
    let connection_manager = Arc::new(ConnectionManager::new(
        usize::MAX,
        100,
        Arc::clone(&metrics),
        coordinator.clone(),
        false,
        (u32::MAX, tokio::time::Duration::from_secs(60)),
    ));
    let room_coordinator: Arc<dyn RoomOperationCoordinatorTrait> =
        Arc::new(crate::coordination::InMemoryRoomOperationCoordinator::new(
            coordinator.clone(),
            database.clone() as Arc<dyn GameDatabase>,
            None,
        ));
    let (_drain_tx, drain_rx) = watch::channel(false);
    let spectator_service = SpectatorService::new(
        database.clone() as Arc<dyn GameDatabase>,
        room_coordinator,
        coordinator.clone(),
        Arc::new(crate::distributed::InMemoryDistributedLock::new()),
        metrics,
        Arc::new(DashMap::new()),
        ProtocolConfig::default(),
        None,
        connection_manager,
        Arc::new(RoomRateLimiter::new(
            crate::rate_limit::RateLimitConfig::default(),
        )),
        Arc::new(AtomicU64::new(0)),
        drain_rx,
    );

    (spectator_service, room, creator_id, coordinator, database)
}

async fn connect_spectator(service: &SpectatorService, player_id: PlayerId, port: u16) {
    let (sender, _receiver) = tokio::sync::mpsc::channel(1);
    service
        .connection_manager
        .connect_test_client(
            player_id,
            sender,
            format!("127.0.0.1:{port}")
                .parse()
                .expect("test socket address"),
        )
        .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn join_tracks_room_membership_and_notifies_players() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_010).await;

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
#[cfg_attr(miri, ignore)]
async fn spectator_join_rejects_seated_and_duplicate_roles_before_mutation() {
    let (service, room, creator_id, _coordinator, database) = setup_service().await;
    let (creator_tx, _creator_rx) = tokio::sync::mpsc::channel(1);
    service
        .connection_manager
        .connect_test_client(
            creator_id,
            creator_tx,
            "127.0.0.1:35002".parse().expect("test socket address"),
        )
        .await;
    service
        .connection_manager
        .assign_client_to_room(&creator_id, room.id)
        .await;

    let seated_error = service
        .join(
            &creator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Seated Watcher".to_string(),
        )
        .await
        .expect_err("a seated player cannot also spectate");
    assert_eq!(seated_error.code, Some(ErrorCode::SpectatorNotAllowed));
    assert!(!service.is_spectating(&creator_id));

    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_011).await;
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Watcher".to_string(),
        )
        .await
        .expect("first spectator join succeeds");
    let duplicate_error = service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Watcher Again".to_string(),
        )
        .await
        .expect_err("a spectator cannot join a second time");
    assert_eq!(duplicate_error.code, Some(ErrorCode::SpectatorJoinFailed));

    let stored = database
        .get_room_spectators(&room.id)
        .await
        .expect("fetch spectators after role rejections");
    assert_eq!(stored.len(), 1, "only the valid spectator is persisted");
    assert_eq!(stored[0].id, spectator_id);
}

/// Service-level pin of the `TOO_MANY_SPECTATORS` admission refusal
/// (issue #396): a room at its configured spectator cap refuses the next
/// admission without publishing a role or mutating the roster, and the
/// freed seat becomes admittable again.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn spectator_join_refuses_when_the_room_is_at_capacity() {
    let (service, room, _creator_id, _coordinator, database) = setup_service().await;
    database
        .set_room_max_spectators(&room.id, Some(1))
        .await
        .expect("capacity update succeeds");

    let first = PlayerId::new_v4();
    connect_spectator(&service, first, 35_014).await;
    service
        .join(
            &first,
            room.game_name.clone(),
            room.code.clone(),
            "First Watcher".to_string(),
        )
        .await
        .expect("the first spectator is admitted");

    let second = PlayerId::new_v4();
    connect_spectator(&service, second, 35_015).await;
    let error = service
        .join(
            &second,
            room.game_name.clone(),
            room.code.clone(),
            "Second Watcher".to_string(),
        )
        .await
        .expect_err("a room at its spectator cap must refuse the next admission");
    assert_eq!(
        error.code,
        Some(ErrorCode::TooManySpectators),
        "the refusal must name the cap: {error:?}"
    );
    assert!(
        !service.is_spectating(&second),
        "the refusal must not publish a spectator role"
    );
    assert_eq!(
        database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch spectators after the refusal")
            .len(),
        1,
        "the refusal must not mutate the roster"
    );

    service.leave(&first).await.expect("spectator leaves");
    service
        .join(
            &second,
            room.game_name.clone(),
            room.code.clone(),
            "Second Watcher".to_string(),
        )
        .await
        .expect("the freed seat is admittable again");
}

/// A panic between the durable spectator admission and the local role
/// publication must be compensated. Uncompensated, the ghost row consumes
/// spectator capacity and is invisible to both maintenance sweeps: no
/// local role (`prune_missing_rooms`) and no pending-repair key
/// (`retry_disconnected_detaches`) ever exists for it.
///
/// The panic fires BEFORE the delivery generation advances, and the
/// connection already ran a prior join/leave cycle: the compensation must
/// roll the row back WITHOUT rewinding the delivery state — an
/// unconditional rollback would resurrect a retired generation (Bugbot
/// finding on PR #459).
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn panicked_join_rolls_back_the_durable_admission() {
    let (service, room, _creator_id, _coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_012).await;

    // A prior join/leave cycle gives the delivery state a retired
    // generation that a naive unconditional rollback would resurrect.
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Cycled Watcher".to_string(),
        )
        .await
        .expect("first spectator join succeeds");
    service
        .leave(&spectator_id)
        .await
        .expect("spectator leaves");
    let generation_before = service
        .connection_manager
        .delivery_generation_for_test(&spectator_id)
        .expect("connected client has delivery state");

    service.panic_spectator_join_for_test(SpectatorJoinPanicPoint::JoinAfterDurableAdmission);

    let outcome = service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Ghost".to_string(),
        )
        .await;
    assert!(
        outcome.is_err(),
        "a panicked join must surface a terminal failure"
    );

    let stored = database
        .get_room_spectators(&room.id)
        .await
        .expect("fetch spectators after panicked join");
    assert!(
        !stored.iter().any(|info| info.id == spectator_id),
        "a panicked join must not leave a durable ghost row, got: {stored:?}"
    );
    assert!(
        !service.is_spectating(&spectator_id),
        "no local role may be published for a panicked join"
    );
    assert_eq!(
        service
            .connection_manager
            .delivery_generation_for_test(&spectator_id),
        Some(generation_before),
        "compensation must not rewind a delivery generation this transaction never advanced"
    );
}

/// Same ghost-row window, but the panic fires AFTER the delivery
/// generation advanced: here the compensation must roll the generation
/// back too, restoring the exact pre-join delivery state.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn panicked_join_after_advance_rolls_back_row_and_generation() {
    let (service, room, _creator_id, _coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_013).await;
    let generation_before = service
        .connection_manager
        .delivery_generation_for_test(&spectator_id)
        .expect("connected client has delivery state");

    service.panic_spectator_join_for_test(SpectatorJoinPanicPoint::JoinAfterGenerationAdvance);

    let outcome = service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Advanced Ghost".to_string(),
        )
        .await;
    assert!(outcome.is_err(), "panicked join must report failure");

    let stored = database
        .get_room_spectators(&room.id)
        .await
        .expect("fetch spectators after panicked post-advance join");
    assert!(
        !stored.iter().any(|info| info.id == spectator_id),
        "a panicked join must not leave a durable ghost row, got: {stored:?}"
    );
    assert!(!service.is_spectating(&spectator_id));
    assert_eq!(
        service
            .connection_manager
            .delivery_generation_for_test(&spectator_id),
        Some(generation_before),
        "a post-advance panic must roll the delivery generation back to the pre-join state"
    );
}

async fn connect_delivered_spectator(
    service: &SpectatorService,
    coordinator: &RecordingCoordinator,
) -> (
    PlayerId,
    crate::coordination::outbound_queue::OutboundReceiver,
) {
    coordinator
        .deliver_registered
        .store(true, Ordering::Release);
    let (sender, receiver) = crate::coordination::outbound_queue::channel(1, 4);
    sender.set_protocol_version(3);
    let player_id = service
        .connection_manager
        .register_classified_client(
            crate::coordination::DeliverySender::classified(sender),
            crate::coordination::ConnectionCloseSignal::detached(),
            "127.0.0.1:35039".parse().expect("socket address"),
            Uuid::new_v4(),
        )
        .await
        .expect("classified client connects");
    (player_id, receiver)
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn missing_room_detach_registration_panic_preserves_correlated_acknowledgement() {
    for timing in [1, 2] {
        let (service, room, _, coordinator, database) = setup_service().await;
        let (spectator_id, mut receiver) =
            connect_delivered_spectator(&service, &coordinator).await;
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Watcher".into(),
            )
            .await
            .expect("join succeeds");
        let baseline = receiver.try_recv();
        assert!(
            matches!(baseline.expect("receive spectator baseline").payload, crate::coordination::outbound_queue::OutboundPayload::Message(message) if matches!(message.as_ref(), ServerMessage::SpectatorJoined(payload) if payload.room_id == room.id && payload.spectator_id == spectator_id))
        );
        database.delete_room(&room.id).await.expect("delete room");
        coordinator.sent.lock().await.clear();
        let before = service
            .connection_manager
            .delivery_generation_for_test(&spectator_id)
            .expect("live generation");
        coordinator
            .panic_registration
            .store(timing, Ordering::Release);
        let operation_id = Uuid::new_v4();
        let outcome = service
            .leave_operation_from_lifecycle(&spectator_id, operation_id, None)
            .await;
        assert!(
            outcome.is_ok(),
            "missing-room detach must finish after registration panic"
        );
        assert!(!service.is_spectating(&spectator_id));
        let after = service
            .connection_manager
            .delivery_generation_for_test(&spectator_id)
            .expect("live generation");
        assert_eq!(after.1, Some(before.0), "missing-room detach advances once");
        let acknowledgement = receiver
            .try_recv()
            .expect("receive correlated acknowledgement through republished handle");
        assert!(matches!(
            acknowledgement.payload,
            crate::coordination::outbound_queue::OutboundPayload::Message(_)
        ));
        let messages = coordinator.messages_for(&spectator_id).await;
        assert!(
            matches!(messages.as_slice(), [ServerMessage::RoomOperationResult { operation_id: id, result }] if *id == operation_id && matches!(result.as_ref(), crate::protocol::RoomOperationResult::SpectatorLeft { room_id: Some(id), room_code: None, current_spectators, .. } if *id == room.id && current_spectators.is_empty())),
            "missing-room detach must acknowledge the correlated operation once: {messages:?}"
        );
    }
}

// Issue #887: every committed removal needs one live and replayed terminal roster.
async fn assert_panicked_detach_terminal_events(
    point: Option<SpectatorDetachPanicPoint>,
    registration_fault: RegistrationFault,
) {
    let (mut service, room, creator_id, coordinator, database) = setup_service().await;
    let replay = Arc::new(ReconnectionManager::new(
        60,
        16,
        Arc::clone(&service.metrics),
    ));
    // Open the replay ring, as a pending reconnection does in production.
    replay.buffer_event(&room.id, ServerMessage::Pong).await;
    service.reconnection_manager = Some(Arc::clone(&replay));
    let (spectator_id, mut receiver) = connect_delivered_spectator(&service, &coordinator).await;
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Watcher".into(),
        )
        .await
        .expect("join succeeds");
    let baseline = receiver.try_recv();
    assert!(
        matches!(baseline.expect("receive spectator baseline").payload, crate::coordination::outbound_queue::OutboundPayload::Message(message) if matches!(message.as_ref(), ServerMessage::SpectatorJoined(payload) if payload.room_id == room.id && payload.spectator_id == spectator_id)),
        "baseline must identify this admission"
    );
    coordinator.sent.lock().await.clear();
    let before = service
        .connection_manager
        .delivery_generation_for_test(&spectator_id)
        .expect("live generation");
    let registrations_before = coordinator.registration_calls.load(Ordering::Acquire);
    if let Some(point) = point {
        service.panic_spectator_detach_for_test(point);
    }
    coordinator
        .panic_registration
        .store(registration_fault as u8, Ordering::Release);
    let operation_id = Uuid::new_v4();
    service
        .leave_operation_from_lifecycle(&spectator_id, operation_id, None)
        .await
        .expect("committed detach succeeds");
    assert!(!service.is_spectating(&spectator_id));
    assert!(database
        .get_room_spectators(&room.id)
        .await
        .expect("stored roster")
        .is_empty());
    let after = service
        .connection_manager
        .delivery_generation_for_test(&spectator_id)
        .expect("live generation");
    assert_ne!(before.0, after.0, "detach must advance generation");
    assert_eq!(
        after.1,
        Some(before.0),
        "{point:?}/{registration_fault:?}: detach must advance generation exactly once"
    );
    if !matches!(registration_fault, RegistrationFault::None) {
        assert_eq!(
            coordinator.registration_calls.load(Ordering::Acquire) - registrations_before,
            2,
            "panicked registration must retry the committed handle"
        );
    }
    let corrections = coordinator.messages_for(&creator_id).await.into_iter().filter(|message| matches!(message, ServerMessage::SpectatorDisconnected { spectator_id: id, spectator_count: Some(0), current_spectators, reason: Some(SpectatorStateChangeReason::VoluntaryLeave) } if *id == spectator_id && current_spectators.is_empty())).count();
    assert_eq!(
        corrections, 1,
        "{point:?}: durable removal must publish one terminal roster"
    );
    if matches!(
        registration_fault,
        RegistrationFault::AlwaysPanicBefore
            | RegistrationFault::AlwaysPanicAfter
            | RegistrationFault::AlwaysError
    ) {
        assert_eq!(
            service
                .connection_manager
                .close_signal_for(&spectator_id)
                .expect("live close signal")
                .requested_reason(),
            Some(crate::coordination::CloseReason::Unregistered)
        );
        assert!(
            coordinator.messages_for(&spectator_id).await.is_empty(),
            "failed publication must not acknowledge through a stale handle"
        );
        let unexpected = receiver.try_recv();
        assert!(
            matches!(
                unexpected,
                Err(crate::coordination::outbound_queue::TryReceiveError::Empty)
            ),
            "failed publication must enqueue no acknowledgement: {unexpected:?}"
        );
    } else {
        let acknowledgement = receiver
            .try_recv()
            .expect("receive acknowledgement through advanced handle");
        assert!(
            matches!(acknowledgement.payload, crate::coordination::outbound_queue::OutboundPayload::Message(message) if matches!(message.as_ref(), ServerMessage::RoomOperationResult { operation_id: id, result } if *id == operation_id && matches!(result.as_ref(), crate::protocol::RoomOperationResult::SpectatorLeft { room_id: Some(id), reason: Some(SpectatorStateChangeReason::VoluntaryLeave), current_spectators, .. } if *id == room.id && current_spectators.is_empty())))
        );
        assert_eq!(
            coordinator.messages_for(&spectator_id).await.len(),
            1,
            "committed leave must acknowledge once"
        );
    }
    let events = replay.get_missed_events(&room.id, 0).await;
    assert_eq!(events.events.iter().filter(|event| matches!(event, ServerMessage::SpectatorDisconnected { spectator_id: id, .. } if *id == spectator_id)).count(), 1, "{point:?}: terminal roster must be replayable once");
    assert_eq!(service.retry_disconnected_detaches().await, 0);
    let unexpected = receiver.try_recv();
    assert!(
        matches!(
            unexpected,
            Err(crate::coordination::outbound_queue::TryReceiveError::Empty)
        ),
        "maintenance must not send another acknowledgement: {unexpected:?}"
    );
    assert_eq!(
        coordinator.messages_for(&creator_id).await.len(),
        1,
        "maintenance must not duplicate the correction"
    );
    if matches!(
        registration_fault,
        RegistrationFault::None | RegistrationFault::PanicBefore | RegistrationFault::PanicAfter
    ) {
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Returned".into(),
            )
            .await
            .expect("recovered connection re-spectates");
        let baseline = receiver.try_recv();
        assert!(
            matches!(baseline.expect("rejoin baseline through next generation").payload, crate::coordination::outbound_queue::OutboundPayload::Message(message) if matches!(message.as_ref(), ServerMessage::SpectatorJoined(payload) if payload.room_id == room.id && payload.spectator_id == spectator_id))
        );
    }
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn panicked_detach_after_remove_preserves_terminal_roster_and_replay() {
    assert_panicked_detach_terminal_events(
        Some(SpectatorDetachPanicPoint::DetachAfterDurableRemove),
        RegistrationFault::None,
    )
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn panicked_detach_after_local_preserves_terminal_roster_and_replay() {
    assert_panicked_detach_terminal_events(
        Some(SpectatorDetachPanicPoint::DetachAfterLocalCompletion),
        RegistrationFault::None,
    )
    .await;
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn panicked_detach_registration_preserves_generation_and_terminal_events() {
    for timing in [
        RegistrationFault::PanicBefore,
        RegistrationFault::PanicAfter,
        RegistrationFault::AlwaysPanicBefore,
        RegistrationFault::AlwaysPanicAfter,
        RegistrationFault::AlwaysError,
    ] {
        assert_panicked_detach_terminal_events(None, timing).await;
    }
}

/// Issue #887: cancellation cannot drop the recovered terminal event or
/// release its room lane before a later admission.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn recovered_detach_keeps_fifo_order_after_caller_cancellation() {
    let (mut service, room, creator_id, coordinator, database) = setup_service().await;
    let replay = Arc::new(ReconnectionManager::new(
        60,
        16,
        Arc::clone(&service.metrics),
    ));
    replay.buffer_event(&room.id, ServerMessage::Pong).await;
    service.reconnection_manager = Some(Arc::clone(&replay));
    let (spectator_id, mut receiver) = connect_delivered_spectator(&service, &coordinator).await;
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "First".into(),
        )
        .await
        .expect("first admission");
    let baseline = receiver.try_recv();
    assert!(
        matches!(baseline.expect("first baseline").payload, crate::coordination::outbound_queue::OutboundPayload::Message(message) if matches!(message.as_ref(), ServerMessage::SpectatorJoined(payload) if payload.spectator_id == spectator_id))
    );
    coordinator.sent.lock().await.clear();
    coordinator
        .delay_room_closed_sends
        .store(true, Ordering::Release);
    service.panic_spectator_detach_for_test(SpectatorDetachPanicPoint::DetachAfterDurableRemove);
    let service = Arc::new(service);
    let leaving = Arc::clone(&service);
    let leave_caller = tokio::spawn(async move { leaving.leave(&spectator_id).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while coordinator
            .room_closed_sends_started
            .load(Ordering::Acquire)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("recovered detach parks at acknowledgement");
    leave_caller.abort();
    assert!(leave_caller
        .await
        .expect_err("caller is cancelled")
        .is_cancelled());
    let (second_id, mut second_receiver) =
        connect_delivered_spectator(&service, &coordinator).await;
    let joining = Arc::clone(&service);
    let game_name = room.game_name.clone();
    let room_code = room.code.clone();
    let later_join = tokio::spawn(async move {
        joining
            .join(&second_id, game_name, room_code, "Second".into())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while coordinator.mutation_lock_attempts.load(Ordering::Acquire) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("later admission reaches held room lane");
    assert!(!later_join.is_finished());
    assert!(database
        .get_room_spectators(&room.id)
        .await
        .expect("parked roster")
        .is_empty());
    coordinator.release_room_closed_sends.add_permits(1);
    later_join
        .await
        .expect("later task completes")
        .expect("later admission succeeds");
    let second_baseline = second_receiver.try_recv();
    assert!(
        matches!(second_baseline.expect("second baseline").payload, crate::coordination::outbound_queue::OutboundPayload::Message(message) if matches!(message.as_ref(), ServerMessage::SpectatorJoined(payload) if payload.spectator_id == second_id))
    );
    let acknowledgement = receiver.try_recv();
    assert!(
        matches!(acknowledgement.expect("recovered acknowledgement").payload, crate::coordination::outbound_queue::OutboundPayload::Message(message) if matches!(message.as_ref(), ServerMessage::SpectatorLeft { room_id: Some(id), current_spectators, .. } if *id == room.id && current_spectators.is_empty()))
    );
    let messages = coordinator.messages_for(&creator_id).await;
    assert!(
        matches!(messages.as_slice(), [ServerMessage::SpectatorDisconnected { spectator_id: id, current_spectators: empty, .. }, ServerMessage::NewSpectatorJoined { spectator, current_spectators, .. }] if *id == spectator_id && empty.is_empty() && spectator.id == second_id && current_spectators.len() == 1),
        "terminal roster must precede later admission: {messages:?}"
    );
    let replayed = replay.get_missed_events(&room.id, 0).await;
    assert!(
        matches!(replayed.events.as_slice(), [ServerMessage::Pong, ServerMessage::NewSpectatorJoined { .. }, ServerMessage::SpectatorDisconnected { spectator_id: id, .. }, ServerMessage::NewSpectatorJoined { spectator, .. }] if *id == spectator_id && spectator.id == second_id),
        "replay must preserve the same FIFO order: {:?}",
        replayed.events
    );
    let unexpected = receiver.try_recv();
    assert!(matches!(
        unexpected,
        Err(crate::coordination::outbound_queue::TryReceiveError::Empty)
    ));
}

/// A v3 spectator needs the same sender `(epoch, seq)` baseline as a seated player.
/// The snapshot is populated from live connection state; the socket send
/// layer strips it again for v2 recipients.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn spectator_snapshot_carries_live_player_epoch() {
    let (service, room, creator_id, coordinator, _database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    let (creator_tx, _creator_rx) = tokio::sync::mpsc::channel(1);
    service
        .connection_manager
        .connect_test_client(
            creator_id,
            creator_tx,
            "127.0.0.1:35001".parse().expect("test socket address"),
        )
        .await;
    service
        .connection_manager
        .assign_client_to_room(&creator_id, room.id)
        .await;
    for expected_seq in 1..=3 {
        assert_eq!(
            service
                .connection_manager
                .next_relay_stamp_in_room(&creator_id, &room.id),
            Some(crate::server::connection_manager::RelayStamp {
                epoch: 1,
                seq: expected_seq,
            })
        );
    }
    let (spectator_tx, _spectator_rx) = tokio::sync::mpsc::channel(1);
    service
        .connection_manager
        .connect_test_client(
            spectator_id,
            spectator_tx,
            "127.0.0.1:35003".parse().expect("test socket address"),
        )
        .await;
    let protocol = crate::server::connection_manager::NegotiatedProtocol {
        version: 3,
        ..Default::default()
    };
    service
        .connection_manager
        .set_protocol(&spectator_id, protocol);

    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Snapshot Watcher".to_string(),
        )
        .await
        .expect("spectator join succeeds");

    let payload = coordinator
        .messages_for(&spectator_id)
        .await
        .into_iter()
        .find_map(|message| match message {
            ServerMessage::SpectatorJoined(payload) => Some(payload),
            _ => None,
        })
        .expect("spectator receives a SpectatorJoined snapshot");

    assert!(
        !payload.current_players.is_empty(),
        "the snapshot lists the room's existing members"
    );
    let creator = payload
        .current_players
        .iter()
        .find(|player| player.id == creator_id)
        .expect("snapshot includes creator");
    assert_eq!((creator.epoch, creator.seq), (Some(1), Some(3)));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn spectator_snapshot_excludes_database_only_pending_join() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    let (creator_tx, _creator_rx) = tokio::sync::mpsc::channel(1);
    service
        .connection_manager
        .connect_test_client(
            creator_id,
            creator_tx,
            "127.0.0.1:35004".parse().expect("test socket address"),
        )
        .await;
    service
        .connection_manager
        .assign_client_to_room(&creator_id, room.id)
        .await;

    let pending_id = PlayerId::new_v4();
    let mut pending = room
        .players
        .get(&creator_id)
        .expect("creator record")
        .clone();
    pending.id = pending_id;
    pending.name = "Pending Player".to_string();
    pending.is_authority = false;
    assert!(
        database
            .add_player_to_room(&room.id, pending)
            .await
            .expect("stage database-only member"),
        "pending member fits in room"
    );

    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_005).await;
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Published Snapshot".to_string(),
        )
        .await
        .expect("spectator join succeeds");

    let payload = coordinator
        .messages_for(&spectator_id)
        .await
        .into_iter()
        .find_map(|message| match message {
            ServerMessage::SpectatorJoined(payload) => Some(payload),
            _ => None,
        })
        .expect("spectator baseline");
    let ids: HashSet<PlayerId> = payload
        .current_players
        .iter()
        .map(|player| player.id)
        .collect();
    assert!(ids.contains(&creator_id), "published creator is present");
    assert!(
        !ids.contains(&pending_id),
        "database membership is invisible until room routing publishes it"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn delayed_spectator_rosters_stay_in_room_fifo_order() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    coordinator.delay_first_spectator_broadcast();
    let service = Arc::new(service);
    let first_id = PlayerId::new_v4();
    let second_id = PlayerId::new_v4();
    connect_spectator(&service, first_id, 35_006).await;
    connect_spectator(&service, second_id, 35_007).await;

    let first_join = {
        let service = Arc::clone(&service);
        let game_name = room.game_name.clone();
        let room_code = room.code.clone();
        tokio::spawn(async move {
            service
                .join(&first_id, game_name, room_code, "First Watcher".to_string())
                .await
        })
    };
    coordinator
        .first_spectator_broadcast_started
        .notified()
        .await;

    let second_join = {
        let service = Arc::clone(&service);
        let game_name = room.game_name.clone();
        let room_code = room.code.clone();
        tokio::spawn(async move {
            service
                .join(
                    &second_id,
                    game_name,
                    room_code,
                    "Second Watcher".to_string(),
                )
                .await
        })
    };

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while coordinator.mutation_lock_attempts.load(Ordering::Acquire) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second admission should reach the occupied room mutation gate");
    assert_eq!(
        database
            .get_room_spectators(&room.id)
            .await
            .expect("read blocked spectator roster")
            .len(),
        1,
        "the second DB mutation waits until the first lifecycle event commits"
    );
    assert!(
        !second_join.is_finished(),
        "the complete second admission waits behind the delayed first room event"
    );

    coordinator.release_first_spectator_broadcast.notify_one();
    first_join
        .await
        .expect("first join task")
        .expect("first spectator joins");
    second_join
        .await
        .expect("second join task")
        .expect("second spectator joins");

    let roster_sizes: Vec<usize> = coordinator
        .messages_for(&creator_id)
        .await
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::NewSpectatorJoined {
                current_spectators, ..
            } => Some(current_spectators.len()),
            _ => None,
        })
        .collect();
    assert_eq!(
        roster_sizes,
        vec![1, 2],
        "full-roster events must follow mutation order even when the first delivery stalls"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn leave_detaches_spectator_and_sends_disconnect_notifications() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    let remaining_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_012).await;
    connect_spectator(&service, remaining_id, 35_013).await;

    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Spectator One".to_string(),
        )
        .await
        .expect("spectator join succeeds");
    service
        .join(
            &remaining_id,
            room.game_name.clone(),
            room.code.clone(),
            "Spectator Two".to_string(),
        )
        .await
        .expect("remaining spectator join succeeds");

    service.leave(&spectator_id).await.expect("leave succeeds");

    assert!(
        service.spectator_rooms.get(&spectator_id).is_none(),
        "spectator mapping should be cleared after leaving"
    );

    let stored_spectators = database
        .get_room_spectators(&room.id)
        .await
        .expect("fetch spectators after leave");
    assert_eq!(
        stored_spectators
            .iter()
            .map(|spectator| spectator.id)
            .collect::<Vec<_>>(),
        vec![remaining_id],
        "detach preserves the authoritative remaining roster"
    );

    let spectator_messages = coordinator.messages_for(&spectator_id).await;
    assert!(
        spectator_messages.into_iter().any(|message| matches!(
            message,
            ServerMessage::SpectatorLeft {
                room_id: Some(left_room),
                reason: Some(SpectatorStateChangeReason::VoluntaryLeave),
                current_spectators,
                ..
            } if left_room == room.id
                && current_spectators.iter().map(|spectator| spectator.id).eq([remaining_id])
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
                current_spectators,
                ..
            } if sid == spectator_id
                && current_spectators.iter().map(|spectator| spectator.id).eq([remaining_id])
        )),
        "players should see SpectatorDisconnected with voluntary leave reason"
    );
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn detach_storage_error_preserves_role_roster_and_retryability() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_014).await;
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Retryable Spectator".to_string(),
        )
        .await
        .expect("spectator join succeeds");
    coordinator.sent.lock().await.clear();
    database.fail_remove_spectator_from_room_for_test(true);

    let error = service
        .leave(&spectator_id)
        .await
        .expect_err("injected storage failure rejects detach");

    assert_eq!(error.code, Some(ErrorCode::StorageError));
    assert!(service.is_spectating(&spectator_id));
    assert!(database
        .get_room_spectators(&room.id)
        .await
        .expect("fetch unchanged roster")
        .iter()
        .any(|spectator| spectator.id == spectator_id));
    assert!(coordinator.messages_for(&spectator_id).await.is_empty());
    assert!(coordinator.messages_for(&creator_id).await.is_empty());

    database.fail_remove_spectator_from_room_for_test(false);
    service
        .leave(&spectator_id)
        .await
        .expect("unchanged role state allows a clean retry");
    assert!(!service.is_spectating(&spectator_id));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn absent_spectator_row_converges_local_role_and_peer_roster() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_015).await;
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Converging Spectator".to_string(),
        )
        .await
        .expect("spectator join succeeds");
    coordinator.sent.lock().await.clear();
    database
        .remove_spectator_from_room(&room.id, &spectator_id)
        .await
        .expect("external spectator removal succeeds")
        .expect("test spectator existed in storage");

    service
        .leave(&spectator_id)
        .await
        .expect("authoritative absence converges as a completed detach");

    assert!(!service.is_spectating(&spectator_id));
    assert!(database
        .get_room_spectators(&room.id)
        .await
        .expect("fetch converged roster")
        .iter()
        .all(|spectator| spectator.id != spectator_id));
    assert!(coordinator
        .messages_for(&spectator_id)
        .await
        .into_iter()
        .any(|message| matches!(
            message,
            ServerMessage::SpectatorLeft {
                room_id: Some(left_room),
                current_spectators,
                ..
            } if left_room == room.id && current_spectators.is_empty()
        )));
    assert!(coordinator
        .messages_for(&creator_id)
        .await
        .into_iter()
        .any(|message| matches!(
            message,
            ServerMessage::SpectatorDisconnected {
                spectator_id: departed,
                current_spectators,
                ..
            } if departed == spectator_id && current_spectators.is_empty()
        )));

    service
        .join(
            &spectator_id,
            room.game_name,
            room.code,
            "Converging Spectator".to_string(),
        )
        .await
        .expect("cleared role and delivery generation allow a fresh join");
    assert!(service.is_spectating(&spectator_id));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_prune_missing_room_clears_spectator_role_issue_241() {
    let (service, room, _creator_id, coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_016).await;
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Expired Room Spectator".to_string(),
        )
        .await
        .expect("spectator join succeeds");
    coordinator.sent.lock().await.clear();
    assert!(database
        .delete_room(&room.id)
        .await
        .expect("inactive cleanup surrogate should delete room"));

    let (_drain_tx, drain) = watch::channel(false);
    assert_eq!(service.prune_missing_rooms(drain).await, 1);
    assert!(!service.is_spectating(&spectator_id));
    assert!(coordinator
        .messages_for(&spectator_id)
        .await
        .into_iter()
        .any(|message| matches!(
            message,
            ServerMessage::SpectatorLeft {
                room_id: Some(closed_room),
                room_code: None,
                reason: Some(SpectatorStateChangeReason::RoomClosed),
                ref current_spectators,
            } if closed_room == room.id && current_spectators.is_empty()
        )));

    let error = service
        .join(
            &spectator_id,
            "missing-game".to_string(),
            "ABSENT".to_string(),
            "Can Try Again".to_string(),
        )
        .await
        .expect_err("new admission reaches storage instead of stale-role rejection");
    assert_eq!(error.code, Some(ErrorCode::RoomNotFound));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_prune_deduplicates_same_room_existence_checks_issue_241() {
    let (service, room, _creator_id, _coordinator, database) = setup_service().await;
    for _ in 0..64 {
        service.spectator_rooms.insert(PlayerId::new_v4(), room.id);
    }
    database.reset_get_room_by_id_calls_for_test();
    let (_drain_tx, drain) = watch::channel(false);

    assert_eq!(service.prune_missing_rooms(drain).await, 0);
    assert_eq!(database.get_room_by_id_calls_for_test(), 1);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_missing_room_notices_across_rooms_wait_concurrently_issue_241() {
    const SPECTATOR_COUNT: usize = 8;
    let (service, _room, _creator_id, coordinator, _database) = setup_service().await;
    for _ in 0..SPECTATOR_COUNT {
        service
            .spectator_rooms
            .insert(PlayerId::new_v4(), RoomId::new_v4());
    }
    coordinator
        .delay_room_closed_sends
        .store(true, Ordering::Release);
    let service_for_prune = service.clone();
    let (_drain_tx, drain) = watch::channel(false);
    let prune = tokio::spawn(async move { service_for_prune.prune_missing_rooms(drain).await });

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let started = coordinator.room_closed_send_started.notified();
            if coordinator
                .room_closed_sends_started
                .load(Ordering::Acquire)
                >= SPECTATOR_COUNT
            {
                break;
            }
            started.await;
        }
    })
    .await
    .expect("every missing-room notice must reach its wait concurrently");
    coordinator
        .release_room_closed_sends
        .add_permits(SPECTATOR_COUNT);

    assert_eq!(prune.await.expect("prune task completes"), SPECTATOR_COUNT);
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_stale_prune_snapshot_cannot_detach_rejoined_room_issue_241() {
    let (service, old_room, _creator_id, _coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_017).await;
    service
        .join(
            &spectator_id,
            old_room.game_name.clone(),
            old_room.code.clone(),
            "Moving Watcher".to_string(),
        )
        .await
        .expect("initial spectator join succeeds");
    assert!(database
        .delete_room(&old_room.id)
        .await
        .expect("old room deletion succeeds"));
    database.pause_next_get_room_by_id_for_test();
    let service_for_prune = service.clone();
    let (_drain_tx, drain) = watch::channel(false);
    let prune = tokio::spawn(async move { service_for_prune.prune_missing_rooms(drain).await });
    database.wait_for_paused_get_room_by_id_for_test().await;

    service
        .leave(&spectator_id)
        .await
        .expect("missing old room still permits local role convergence");

    let new_room = database
        .create_room(
            "rejoined-game".to_string(),
            None,
            8,
            true,
            PlayerId::new_v4(),
            "udp".to_string(),
            "region-b".to_string(),
            None,
        )
        .await
        .expect("replacement room creation succeeds");
    service
        .join(
            &spectator_id,
            new_room.game_name.clone(),
            new_room.code.clone(),
            "Moved Watcher".to_string(),
        )
        .await
        .expect("spectator rejoins replacement room");
    database.release_paused_get_room_by_id_for_test();

    assert_eq!(prune.await.expect("prune task completes"), 0);
    assert_eq!(service.spectator_room(&spectator_id), Some(new_room.id));
    let stored = database
        .get_room_by_id(&new_room.id)
        .await
        .expect("replacement room lookup succeeds")
        .expect("replacement room remains");
    assert!(stored.spectators.contains_key(&spectator_id));
}

#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn test_prune_suppresses_room_closed_notice_after_drain_issue_241() {
    let (service, room, _creator_id, coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_018).await;
    service.spectator_rooms.insert(spectator_id, room.id);
    coordinator.sent.lock().await.clear();
    assert!(database
        .delete_room(&room.id)
        .await
        .expect("inactive cleanup surrogate should delete room"));
    let (drain_tx, drain) = watch::channel(false);
    drain_tx.send(true).expect("drain receiver remains live");

    assert_eq!(service.prune_missing_rooms(drain).await, 1);
    assert!(!service.is_spectating(&spectator_id));
    assert!(coordinator.messages_for(&spectator_id).await.is_empty());
}

/// The drain gate must also suppress the BROADCAST side of a detach on a
/// live room: a routed member receives no `SpectatorDisconnected` when the
/// detach runs under an active drain (the room-gone prune branch above
/// never fans out at all, so it cannot cover this path).
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn detach_under_drain_suppresses_the_member_broadcast() {
    for point in [
        None,
        Some(SpectatorDetachPanicPoint::DetachAfterDurableRemove),
        Some(SpectatorDetachPanicPoint::DetachAfterLocalCompletion),
    ] {
        let (service, room, creator_id, coordinator, database) = setup_service().await;
        let spectator_id = PlayerId::new_v4();
        connect_spectator(&service, spectator_id, 35_020).await;
        service
            .join(
                &spectator_id,
                room.game_name.clone(),
                room.code.clone(),
                "Drained Watcher".to_string(),
            )
            .await
            .expect("spectator join succeeds");

        // Route a live room member so the fan-out has a real recipient.
        let (member_tx, _member_rx) = tokio::sync::mpsc::channel(16);
        service
            .connection_manager
            .connect_test_client(
                creator_id,
                member_tx,
                "127.0.0.1:35021".parse().expect("test socket address"),
            )
            .await;
        service
            .connection_manager
            .assign_client_to_room(&creator_id, room.id)
            .await;

        let (drain_tx, drain) = watch::channel(false);
        drain_tx.send(true).expect("drain receiver remains live");
        coordinator.sent.lock().await.clear();

        // The disconnect-time detach path (`detach_if`) carries the live
        // server drain receiver; persistence convergence is deliberately not
        // drain-gated, only the notifications are.
        if let Some(point) = point {
            service.panic_spectator_detach_for_test(point);
        }
        let send_notifications = || true;
        assert!(
            service
                .detach_if(
                    &spectator_id,
                    SpectatorStateChangeReason::Disconnected,
                    &send_notifications,
                    drain,
                )
                .await
        );
        assert!(!service.is_spectating(&spectator_id));
        let stored = database
            .get_room_spectators(&room.id)
            .await
            .expect("fetch spectators after drained detach");
        assert!(
            !stored.iter().any(|info| info.id == spectator_id),
            "persistence must converge even under drain"
        );

        // Neither the departed spectator nor the routed member may receive a
        // drained-detach notification.
        assert!(coordinator.messages_for(&spectator_id).await.is_empty());
        assert!(
            coordinator.messages_for(&creator_id).await.is_empty(),
            "a routed member must not observe a SpectatorDisconnected suppressed by drain"
        );
    }
}

/// A pending unpublished-detach row whose identity has since been
/// republished to the same room is void, not executable: the maintenance
/// sweep must clear the rollback record while leaving the live roster row
/// and the role mapping alone. Executing it instead would ghost a seated
/// broadcast out of a spectator that the room still lists.
#[tokio::test]
#[cfg_attr(miri, ignore)]
async fn retry_disconnected_detaches_never_deletes_a_republished_identity() {
    let (service, room, _creator_id, _coordinator, database) = setup_service().await;
    let spectator_id = PlayerId::new_v4();
    connect_spectator(&service, spectator_id, 35_019).await;
    service
        .join(
            &spectator_id,
            room.game_name.clone(),
            room.code.clone(),
            "Republished Watcher".to_string(),
        )
        .await
        .expect("spectator join succeeds");

    // Simulate the stale rollback state: a disconnect-time storage failure
    // left an unpublished detach row behind, then the same durable
    // identity was re-admitted to the same room before maintenance ran.
    service.pending_unpublished_detaches.insert(
        (room.id, spectator_id),
        PendingSpectatorDetachRepair::RowDelete,
    );

    assert_eq!(
        service.retry_disconnected_detaches().await,
        0,
        "voiding a republished row is not a detach"
    );
    let stored = database
        .get_room_spectators(&room.id)
        .await
        .expect("fetch spectators");
    assert!(
        stored.iter().any(|info| info.id == spectator_id),
        "maintenance must not delete the published roster entry"
    );
    assert_eq!(service.spectator_room(&spectator_id), Some(room.id));
    assert!(
        !service
            .pending_unpublished_detaches
            .contains_key(&(room.id, spectator_id)),
        "the voided rollback record is cleared"
    );
}

/// A ghost row kept by a failed compensating rollback is durable state a
/// racing admission can announce (its absolute baseline or delta includes
/// the ghost). When the sweep deletes the ghost, live members need the
/// absolute correcting event — otherwise the ghost stays in their count
/// until the room's next spectator event, indefinitely for a quiet room.
#[tokio::test]
#[cfg_attr(miri, ignore)]
#[cfg(test)]
async fn retry_disconnected_detaches_publishes_a_correcting_event_for_a_deleted_ghost() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    let ghost_id = PlayerId::new_v4();
    // The failed-rollback residue: a durable roster row plus the pending
    // repair key, with no local role and no live connection to notify.
    database
        .add_spectator_to_room(
            &room.id,
            SpectatorInfo {
                id: ghost_id,
                name: "Ghost Watcher".to_string(),
                connected_at: Some(chrono::Utc::now()),
            },
        )
        .await
        .expect("ghost row insert succeeds");
    service
        .pending_unpublished_detaches
        .insert((room.id, ghost_id), PendingSpectatorDetachRepair::RowDelete);

    assert_eq!(
        service.retry_disconnected_detaches().await,
        1,
        "the ghost row is deleted as a detach"
    );
    let stored = database
        .get_room_spectators(&room.id)
        .await
        .expect("fetch spectators after ghost repair");
    assert!(
        !stored.iter().any(|info| info.id == ghost_id),
        "the ghost row must not survive the sweep"
    );

    let corrections = coordinator
        .messages_for(&creator_id)
        .await
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::SpectatorDisconnected {
                spectator_id,
                spectator_count,
                current_spectators,
                ..
            } if spectator_id == ghost_id => Some((spectator_count, current_spectators)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        corrections.len(),
        1,
        "the ghost repair must publish exactly one correcting SpectatorDisconnected"
    );
    let (spectator_count, current_spectators) = &corrections[0];
    assert_eq!(
        spectator_count,
        &Some(0),
        "the roster is absolute and post-delete: the ghost is gone"
    );
    assert!(
        current_spectators.iter().all(|info| info.id != ghost_id),
        "the corrected roster must not name the ghost"
    );
}

/// The sweep deletes the ghost row before the repair notification reads
/// the room, so a transient storage fault in that read would otherwise
/// lose the correction forever — no row and no repair key left to retry.
/// The read failure must re-queue the repair key, and the next sweep must
/// converge the notification. Repository-gated: the fault-injection hook
/// it drives is `signal_fish_repository_tests`-only, like its siblings.
#[tokio::test]
#[cfg(all(test, signal_fish_repository_tests))]
#[cfg_attr(miri, ignore)]
#[cfg(test)]
async fn ghost_repair_notification_is_retried_after_a_storage_fault() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    let ghost_id = PlayerId::new_v4();
    database
        .add_spectator_to_room(
            &room.id,
            SpectatorInfo {
                id: ghost_id,
                name: "Ghost Watcher".to_string(),
                connected_at: Some(chrono::Utc::now()),
            },
        )
        .await
        .expect("ghost row insert succeeds");
    service
        .pending_unpublished_detaches
        .insert((room.id, ghost_id), PendingSpectatorDetachRepair::RowDelete);

    database.fail_get_room_by_id_for_test(true);
    assert_eq!(
        service.retry_disconnected_detaches().await,
        1,
        "the first sweep still deletes the ghost row"
    );
    assert!(
        service
            .pending_unpublished_detaches
            .contains_key(&(room.id, ghost_id)),
        "a failed notification read must re-queue the repair for the next sweep"
    );
    assert!(
        coordinator.messages_for(&creator_id).await.is_empty(),
        "no correcting event can be built without the room row"
    );

    database.fail_get_room_by_id_for_test(false);
    assert_eq!(
        service.retry_disconnected_detaches().await,
        1,
        "the retry sweep converges the notification"
    );
    let corrections = coordinator
        .messages_for(&creator_id)
        .await
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::SpectatorDisconnected {
                spectator_id,
                spectator_count,
                ..
            } if spectator_id == ghost_id => Some(spectator_count),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        corrections,
        vec![Some(0)],
        "exactly one absolute correction, built after the delete"
    );
    assert!(
        !service
            .pending_unpublished_detaches
            .contains_key(&(room.id, ghost_id)),
        "the converged repair leaves no retry key behind"
    );
}

/// A re-queued notification marker must survive a later successful
/// rollback for the same identity: the rollback clears only its own
/// `RowDelete` repair, so the ghost correction still converges on the
/// next sweep instead of being suppressed by an unrelated rollback.
#[tokio::test]
#[cfg(all(test, signal_fish_repository_tests))]
#[cfg_attr(miri, ignore)]
#[cfg(test)]
async fn ghost_notify_retry_survives_a_later_rollback_for_the_same_identity() {
    let (service, room, creator_id, coordinator, database) = setup_service().await;
    let ghost_id = PlayerId::new_v4();

    // Phase 1: the sweep deletes the ghost row but cannot read the room
    // for the correction; the repair re-queues as a notification retry.
    database
        .add_spectator_to_room(
            &room.id,
            SpectatorInfo {
                id: ghost_id,
                name: "Ghost Watcher".to_string(),
                connected_at: Some(chrono::Utc::now()),
            },
        )
        .await
        .expect("ghost row insert succeeds");
    service
        .pending_unpublished_detaches
        .insert((room.id, ghost_id), PendingSpectatorDetachRepair::RowDelete);
    database.fail_get_room_by_id_for_test(true);
    assert_eq!(service.retry_disconnected_detaches().await, 1);
    database.fail_get_room_by_id_for_test(false);

    // Phase 2: the same identity re-spectates and the join fails after
    // the durable admission; the compensation deletes its own row
    // successfully — and must not clear the notification it does not own.
    connect_spectator(&service, ghost_id, 35_021).await;
    service.panic_spectator_join_for_test(SpectatorJoinPanicPoint::JoinAfterDurableAdmission);
    let outcome = service
        .join(
            &ghost_id,
            room.game_name.clone(),
            room.code.clone(),
            "Rejoining Watcher".to_string(),
        )
        .await;
    assert!(outcome.is_err(), "the injected panic fails the re-join");
    assert_eq!(
        service
            .pending_unpublished_detaches
            .get(&(room.id, ghost_id))
            .map(|entry| *entry.value()),
        Some(PendingSpectatorDetachRepair::NotifyRetry),
        "the successful rollback must not clear the notification retry"
    );

    // Phase 3: the next sweep converges the correction exactly once.
    assert_eq!(service.retry_disconnected_detaches().await, 1);
    let corrections = coordinator
        .messages_for(&creator_id)
        .await
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::SpectatorDisconnected {
                spectator_id,
                spectator_count,
                ..
            } if spectator_id == ghost_id => Some(spectator_count),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        corrections,
        vec![Some(0)],
        "the surviving retry publishes exactly one absolute correction"
    );
    assert!(
        !service
            .pending_unpublished_detaches
            .contains_key(&(room.id, ghost_id)),
        "the converged repair leaves no retry key behind"
    );
}
