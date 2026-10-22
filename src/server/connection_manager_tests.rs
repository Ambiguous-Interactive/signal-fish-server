use super::*;

impl ConnectionManager {
    #[cfg(all(test, signal_fish_repository_tests))]
    pub(super) async fn run_admission_pause_for_test(&self, client_addr: SocketAddr) {
        if let Some((_, (reached, release, fail))) = self.admission_pauses.remove(&client_addr) {
            reached.notify_one();
            release.notified().await;
            if fail {
                panic!("injected admission callback panic");
            }
        }
    }
}
use crate::coordination::{
    CloseReason, MembershipUpdate, MessageCoordinator, RoomEventCompletion, RoomEventJob,
    RoomEventMutationGuard, RoomEventSequencer,
};
use crate::distributed::SequencedMessage;
use anyhow::Result;
use async_trait::async_trait;
use std::net::SocketAddr;
use tokio::sync::{mpsc, Mutex};

#[derive(Default)]
struct TestCoordinator {
    room_events: Arc<RoomEventSequencer>,
    registrations: Mutex<Vec<(PlayerId, Option<RoomId>)>>,
    unregisters: Mutex<Vec<PlayerId>>,
    /// When armed, `try_send_to_player` parks until released
    /// ([`FarewellGate::park`]), so a test can land a concurrent map
    /// mutation inside a farewell await.
    farewell_gate: Option<Arc<FarewellGate>>,
    admission_fault: Option<Arc<AdmissionFault>>,
    cleanup_fault: Mutex<Option<Arc<AdmissionFault>>>,
    cleanup_constructor_panics: std::sync::atomic::AtomicBool,
    active_registrations: Mutex<std::collections::HashSet<PlayerId>>,
}

#[derive(Clone, Copy)]
enum AdmissionBehavior {
    Panic,
    Park,
    Error,
}

struct AdmissionFault {
    behavior: AdmissionBehavior,
    after_effect: bool,
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    player_id: Mutex<Option<PlayerId>>,
}

impl AdmissionFault {
    async fn trigger(&self, player_id: PlayerId) -> Result<()> {
        *self.player_id.lock().await = Some(player_id);
        self.entered.notify_one();
        match self.behavior {
            AdmissionBehavior::Panic => panic!("injected admission callback panic"),
            AdmissionBehavior::Park => {
                self.release.notified().await;
                Ok(())
            }
            AdmissionBehavior::Error => anyhow::bail!("injected ordinary admission error"),
        }
    }
}

/// Parks [`TestCoordinator::try_send_to_player`] until released. The
/// `entered` channel fires once the park is reached; the one-shot release
/// completes the parked send.
struct FarewellGate {
    entered_tx: mpsc::UnboundedSender<()>,
    release: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl FarewellGate {
    fn channel() -> (
        Self,
        mpsc::UnboundedReceiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        (
            Self {
                entered_tx,
                release: std::sync::Mutex::new(Some(release_rx)),
            },
            entered_rx,
            release_tx,
        )
    }

    async fn park(&self) {
        let _ = self.entered_tx.send(());
        let release = self
            .release
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(release) = release {
            let _ = release.await;
        }
    }
}

#[async_trait]
impl MessageCoordinator for TestCoordinator {
    async fn lock_room_event_mutation(&self, room_id: &RoomId) -> RoomEventMutationGuard {
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
        _player_id: &PlayerId,
        _message: Arc<ServerMessage>,
    ) -> Result<()> {
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
        if let Some(gate) = &self.farewell_gate {
            gate.park().await;
        }
        self.send_to_player(player_id, message).await.map(|()| true)
    }

    async fn broadcast_to_room(
        &self,
        _room_id: &RoomId,
        _message: Arc<ServerMessage>,
    ) -> Result<()> {
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
        _expected_members: &[PlayerId],
        message: Arc<ServerMessage>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>
                + Send
                + 'a,
        >,
    ) -> Result<bool> {
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
        _recipient_messages: Vec<crate::coordination::RoomRecipientMessages>,
        _spectator_messages: Vec<Arc<ServerMessage>>,
        before_send: Box<
            dyn FnOnce() -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<bool>> + Send + 'a>,
                > + Send
                + 'a,
        >,
        after_first_phase: Box<dyn FnOnce(usize) -> bool + Send + 'a>,
    ) -> Result<crate::coordination::RoomMessageTransactionOutcome> {
        if before_send().await? {
            let _ = after_first_phase(0);
            Ok(crate::coordination::RoomMessageTransactionOutcome::Committed)
        } else {
            Ok(crate::coordination::RoomMessageTransactionOutcome::HookRejected)
        }
    }

    async fn register_local_client(
        &self,
        player_id: PlayerId,
        room_id: Option<RoomId>,
        _delivery: crate::coordination::ClientDeliveryHandle,
    ) -> Result<()> {
        let fault = self
            .admission_fault
            .as_ref()
            .filter(|fault| fault.armed.swap(false, Ordering::AcqRel));
        if let Some(fault) = fault.filter(|fault| !fault.after_effect) {
            fault.trigger(player_id).await?;
        }
        self.registrations.lock().await.push((player_id, room_id));
        self.active_registrations.lock().await.insert(player_id);
        if let Some(fault) = fault.filter(|fault| fault.after_effect) {
            fault.trigger(player_id).await?;
        }
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
        self.registrations.lock().await.push((player_id, None));
        Ok(Some((epoch, final_seq)))
    }

    fn unregister_local_client<'life0, 'life1, 'async_trait>(
        &'life0 self,
        player_id: &'life1 PlayerId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        if self.cleanup_constructor_panics.load(Ordering::Acquire) {
            panic!("injected cleanup future constructor panic");
        }
        Box::pin(async move {
            self.unregisters.lock().await.push(*player_id);
            let fault = self.cleanup_fault.lock().await.clone();
            if let Some(fault) = fault.as_ref().filter(|fault| !fault.after_effect) {
                fault.trigger(*player_id).await?;
            }
            self.active_registrations.lock().await.remove(player_id);
            if let Some(fault) = fault.as_ref().filter(|fault| fault.after_effect) {
                fault.trigger(*player_id).await?;
            }
            Ok(())
        })
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

fn make_manager(max_connections_per_ip: usize) -> Arc<ConnectionManager> {
    make_limited_manager(usize::MAX, max_connections_per_ip)
}

fn make_limited_manager(
    max_connections: usize,
    max_connections_per_ip: usize,
) -> Arc<ConnectionManager> {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator: Arc<dyn MessageCoordinator> = Arc::new(TestCoordinator::default());
    Arc::new(ConnectionManager::new(
        max_connections,
        max_connections_per_ip,
        metrics,
        coordinator,
        false,
        (u32::MAX, tokio::time::Duration::from_secs(60)),
    ))
}

fn channel() -> (
    mpsc::Sender<Arc<ServerMessage>>,
    mpsc::Receiver<Arc<ServerMessage>>,
) {
    mpsc::channel(4)
}

#[track_caller]
fn expect_reassigned(outcome: ReassignmentOutcome) -> ClientDeliveryHandle {
    match outcome {
        ReassignmentOutcome::Reassigned(delivery) => delivery,
        other => panic!("expected reassignment, got {other:?}"),
    }
}

fn admission_fault_manager(
    behavior: AdmissionBehavior,
    after_effect: bool,
) -> (
    Arc<ConnectionManager>,
    Arc<TestCoordinator>,
    Arc<AdmissionFault>,
) {
    let fault = Arc::new(AdmissionFault {
        behavior,
        after_effect,
        armed: std::sync::atomic::AtomicBool::new(true),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        player_id: Mutex::new(None),
    });
    let coordinator = Arc::new(TestCoordinator {
        admission_fault: Some(Arc::clone(&fault)),
        ..TestCoordinator::default()
    });
    let manager = Arc::new(ConnectionManager::new(
        1,
        1,
        Arc::new(ServerMetrics::new()),
        coordinator.clone(),
        true,
        (u32::MAX, tokio::time::Duration::from_secs(60)),
    ));
    (manager, coordinator, fault)
}

async fn assert_failed_admission_is_released(
    manager: &Arc<ConnectionManager>,
    coordinator: &TestCoordinator,
    fault: &AdmissionFault,
    close: &ConnectionCloseSignal,
) {
    let player_id = fault.player_id.lock().await.expect("callback player id");
    tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        while manager.live_connections.load(Ordering::Acquire) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed admission kept its reserved slot");
    assert!(manager.clients.is_empty());
    assert!(manager.connections_per_ip.is_empty());
    assert_eq!(
        manager.metrics.active_connections.load(Ordering::Relaxed),
        0
    );
    assert!(manager
        .metrics
        .connection_delivery_stats(&player_id)
        .is_none());
    manager.metrics.record_slow_consumer_eviction(&player_id);
    assert!(manager
        .metrics
        .slow_consumer_eviction_attributions_snapshot()
        .is_empty());
    assert!(coordinator.active_registrations.lock().await.is_empty());
    assert!(coordinator.unregisters.lock().await.contains(&player_id));
    assert_eq!(close.requested_reason(), Some(CloseReason::Unregistered));
    let (sender, _receiver) = channel();
    let retry = manager
        .register_client(
            sender,
            ConnectionCloseSignal::detached(),
            "127.0.0.1:5050".parse().unwrap(),
            Uuid::new_v4(),
        )
        .await
        .expect("released admission must permit a retry");
    assert!(manager.clients.contains_key(&retry));
}

// Regression #839: callbacks can unwind before admission returns its identity.
#[tokio::test]
async fn admission_callback_panic_releases_before_and_after_routing_effect() {
    for after_effect in [false, true] {
        let (manager, coordinator, fault) =
            admission_fault_manager(AdmissionBehavior::Panic, after_effect);
        let (close, _listener) = ConnectionCloseSignal::channel();
        let (sender, _receiver) = channel();
        let manager_for_caller = Arc::clone(&manager);
        let close_for_caller = close.clone();
        let caller = tokio::spawn(async move {
            manager_for_caller
                .register_client(
                    sender,
                    close_for_caller,
                    "127.0.0.1:5050".parse().unwrap(),
                    Uuid::new_v4(),
                )
                .await
        });
        assert!(matches!(
            caller.await.expect("callback panic is contained"),
            Err(RegisterClientError::AdmissionFailed)
        ));
        assert_failed_admission_is_released(&manager, &coordinator, &fault, &close).await;
    }
}

// Regression #839: cancellation cannot strand either admission or routing.
#[tokio::test]
async fn canceled_admission_releases_before_and_after_routing_effect() {
    for after_effect in [false, true] {
        let (manager, coordinator, fault) =
            admission_fault_manager(AdmissionBehavior::Park, after_effect);
        let (close, _listener) = ConnectionCloseSignal::channel();
        let (sender, _receiver) = channel();
        let manager_for_caller = Arc::clone(&manager);
        let close_for_caller = close.clone();
        let caller = tokio::spawn(async move {
            manager_for_caller
                .register_client(
                    sender,
                    close_for_caller,
                    "127.0.0.1:5050".parse().unwrap(),
                    Uuid::new_v4(),
                )
                .await
        });
        tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            fault.entered.notified(),
        )
        .await
        .expect("callback parked");
        caller.abort();
        assert!(caller
            .await
            .expect_err("admission caller canceled")
            .is_cancelled());
        assert_failed_admission_is_released(&manager, &coordinator, &fault, &close).await;
    }
}

#[tokio::test]
async fn canceled_admission_reply_before_ack_releases_committed_route() {
    let (manager, coordinator, fault) = admission_fault_manager(AdmissionBehavior::Error, true);
    let (close, _listener) = ConnectionCloseSignal::channel();
    let (sender, _receiver) = channel();
    let mut caller = Box::pin(manager.register_client(
        sender,
        close.clone(),
        "127.0.0.1:5050".parse().unwrap(),
        Uuid::new_v4(),
    ));
    assert!(futures_util::poll!(caller.as_mut()).is_pending());
    tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        manager.admission_reply_sent.notified(),
    )
    .await
    .expect("admission reply sent before caller observed it");
    assert_eq!(manager.live_connections.load(Ordering::Acquire), 1);
    assert_eq!(coordinator.active_registrations.lock().await.len(), 1);
    drop(caller);
    assert_failed_admission_is_released(&manager, &coordinator, &fault, &close).await;
}

#[tokio::test]
async fn pending_admission_close_preserves_identity_and_serializes_external_removal() {
    for after_effect in [false, true] {
        let (manager, coordinator, fault) =
            admission_fault_manager(AdmissionBehavior::Park, after_effect);
        let (close, _listener) = ConnectionCloseSignal::channel();
        let (sender, _receiver) = channel();
        let mut caller = Box::pin(manager.register_client(
            sender,
            close.clone(),
            "127.0.0.1:5050".parse().unwrap(),
            Uuid::new_v4(),
        ));
        assert!(futures_util::poll!(caller.as_mut()).is_pending());
        tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            fault.entered.notified(),
        )
        .await
        .expect("registration callback parked");
        let player_id = fault.player_id.lock().await.unwrap();
        assert!(manager
            .remove_client_for_unregistration(&player_id, || false)
            .is_none());
        assert_eq!(manager.live_connections.load(Ordering::Acquire), 1);
        let lifecycle = manager.client_lifecycle(&player_id).unwrap();
        let mut removal = Box::pin(async {
            let _gate = Arc::clone(&lifecycle).lock_owned().await;
            coordinator
                .unregister_local_client(&player_id)
                .await
                .unwrap();
            let removed = manager.remove_client_for_unregistration(&player_id, || true);
            if removed.is_some() {
                manager.metrics.decrement_active_connections();
            }
            removed
        });
        assert!(futures_util::poll!(removal.as_mut()).is_pending());
        close.request_close(CloseReason::Shutdown);
        let admitted = tokio::time::timeout(tokio::time::Duration::from_secs(5), caller)
            .await
            .expect("physical close stops pending callback")
            .expect("closed identity handoff");
        assert_eq!(admitted, player_id);
        assert!(manager.clients.contains_key(&admitted));
        assert_eq!(close.requested_reason(), Some(CloseReason::Shutdown));
        assert!(
            tokio::time::timeout(tokio::time::Duration::from_secs(5), removal)
                .await
                .unwrap()
                .is_some()
        );
        assert!(manager.clients.is_empty());
        assert!(coordinator.active_registrations.lock().await.is_empty());
        assert_eq!(manager.live_connections.load(Ordering::Acquire), 0);
        assert_eq!(
            manager.metrics.active_connections.load(Ordering::Relaxed),
            0
        );
    }
}

#[tokio::test]
async fn failed_admission_cleanup_preserves_local_rollback_and_structured_failure() {
    for (behavior, constructor_panic) in [
        (AdmissionBehavior::Error, false),
        (AdmissionBehavior::Panic, false),
        (AdmissionBehavior::Panic, true),
    ] {
        for after_effect in [false, true] {
            let (manager, coordinator, _fault) =
                admission_fault_manager(AdmissionBehavior::Panic, true);
            coordinator
                .cleanup_constructor_panics
                .store(constructor_panic, Ordering::Release);
            *coordinator.cleanup_fault.lock().await = Some(Arc::new(AdmissionFault {
                behavior,
                after_effect,
                armed: std::sync::atomic::AtomicBool::new(true),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                player_id: Mutex::new(None),
            }));
            let (sender, _receiver) = channel();
            let (close, _listener) = ConnectionCloseSignal::channel();
            let result = manager
                .register_client(
                    sender,
                    close.clone(),
                    "127.0.0.1:5050".parse().unwrap(),
                    Uuid::new_v4(),
                )
                .await;
            assert!(matches!(result, Err(RegisterClientError::AdmissionFailed)));
            tokio::time::timeout(
                tokio::time::Duration::from_secs(1),
                manager.admission_reply_sent.notified(),
            )
            .await
            .expect("caught cleanup failure sends an explicit admission reply");
            assert!(manager.clients.is_empty());
            assert!(manager.connections_per_ip.is_empty());
            assert_eq!(manager.live_connections.load(Ordering::Acquire), 0);
            assert_eq!(
                manager.metrics.active_connections.load(Ordering::Relaxed),
                0
            );
            assert_eq!(close.requested_reason(), Some(CloseReason::Unregistered));
            // An opaque adapter that fails before removing its route owns
            // that retained state; local rollback cannot repair it.
            assert_eq!(
                coordinator.active_registrations.lock().await.is_empty(),
                after_effect && !constructor_panic
            );
        }
    }
}

#[tokio::test]
async fn blocked_admission_cleanup_retains_budget_until_routing_settles() {
    let (manager, coordinator, fault) = admission_fault_manager(AdmissionBehavior::Park, true);
    let cleanup = Arc::new(AdmissionFault {
        behavior: AdmissionBehavior::Park,
        after_effect: true,
        armed: std::sync::atomic::AtomicBool::new(true),
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        player_id: Mutex::new(None),
    });
    *coordinator.cleanup_fault.lock().await = Some(Arc::clone(&cleanup));
    let (sender, _receiver) = channel();
    let (close, _listener) = ConnectionCloseSignal::channel();
    let mut caller = Box::pin(manager.register_client(
        sender,
        close.clone(),
        "127.0.0.1:5050".parse().unwrap(),
        Uuid::new_v4(),
    ));
    assert!(futures_util::poll!(caller.as_mut()).is_pending());
    tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        fault.entered.notified(),
    )
    .await
    .unwrap();
    drop(caller);
    tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        cleanup.entered.notified(),
    )
    .await
    .unwrap();
    let id = fault.player_id.lock().await.unwrap();
    assert!(manager
        .remove_client_for_unregistration(&id, || false)
        .is_none());
    assert_eq!(manager.live_connections.load(Ordering::Acquire), 1);
    let (retry, _receiver) = channel();
    assert!(matches!(
        manager
            .register_client(
                retry,
                ConnectionCloseSignal::detached(),
                "127.0.0.2:5050".parse().unwrap(),
                Uuid::new_v4(),
            )
            .await,
        Err(RegisterClientError::CapacityExceeded { .. })
    ));
    cleanup.release.notify_one();
    *coordinator.cleanup_fault.lock().await = None;
    assert_failed_admission_is_released(&manager, &coordinator, &fault, &close).await;
}

#[tokio::test]
async fn ordinary_admission_callback_error_keeps_existing_acceptance_policy() {
    for after_effect in [false, true] {
        let (manager, _coordinator, _fault) =
            admission_fault_manager(AdmissionBehavior::Error, after_effect);
        let (sender, _receiver) = channel();
        let player_id = manager
            .register_client(
                sender,
                ConnectionCloseSignal::detached(),
                "127.0.0.1:5050".parse().unwrap(),
                Uuid::new_v4(),
            )
            .await
            .expect("ordinary coordinator errors remain admitted");
        assert!(manager.clients.contains_key(&player_id));
        assert_eq!(manager.live_connections.load(Ordering::Acquire), 1);
        assert_eq!(
            manager.metrics.active_connections.load(Ordering::Relaxed),
            1
        );
    }
}

#[tokio::test]
async fn test_coordinator_uses_unknown_routing_default() {
    let coordinator = TestCoordinator::default();

    assert_eq!(
        coordinator
            .routed_player_ids(&RoomId::new_v4())
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn register_client_enforces_ip_limits_and_releases_on_remove() {
    let manager = make_manager(1);
    let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let (tx1, _rx1) = channel();
    let first_id = manager
        .register_client(tx1, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("first registration succeeds");

    let (tx2, _rx2) = channel();
    let err = manager
        .register_client(tx2, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect_err("second client hits per-IP limit");
    match err {
        RegisterClientError::IpLimitExceeded { current, limit } => {
            assert_eq!(current, 1);
            assert_eq!(limit, 1);
        }
        RegisterClientError::CapacityExceeded { .. } => {
            panic!("per-IP refusal must win when both caps bind at one IP")
        }
        RegisterClientError::ServerDraining => {
            panic!("connection manager does not own shutdown drain admission")
        }
        RegisterClientError::AdmissionFailed => panic!("ordinary coordinator must admit"),
    }

    manager.remove_client(&first_id);

    let (tx3, _rx3) = channel();
    manager
        .register_client(tx3, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registrations resume after slot release");
}

/// Issue #502 item 2: the server-wide ceiling refuses admission no matter
/// how many distinct source IPs are involved (per-IP caps alone are
/// multiplied by IP count), and releases the slot at unregistration so
/// admission resumes. Also pins the slot conservation invariant: exactly
/// `limit` admissions are possible again after every live entry is removed.
#[tokio::test]
async fn global_connection_ceiling_refuses_across_ips_and_releases_on_remove() {
    let manager = make_limited_manager(2, 8);
    let addrs: Vec<SocketAddr> = ["127.0.0.1:6001", "198.51.100.1:6002", "198.51.100.2:6003"]
        .iter()
        .map(|raw| raw.parse().unwrap())
        .collect();

    let mut ids = Vec::new();
    for (i, addr) in addrs.iter().take(2).enumerate() {
        let (tx, _rx) = channel();
        ids.push(
            manager
                .register_client(tx, ConnectionCloseSignal::detached(), *addr, Uuid::new_v4())
                .await
                .unwrap_or_else(|err| {
                    panic!("registration {i} under the global ceiling must succeed: {err:?}")
                }),
        );
    }

    let (tx3, _rx3) = channel();
    match manager
        .register_client(
            tx3,
            ConnectionCloseSignal::detached(),
            addrs[2],
            Uuid::new_v4(),
        )
        .await
    {
        Err(RegisterClientError::CapacityExceeded { current, limit }) => {
            assert_eq!((current, limit), (2, 2));
        }
        other => panic!("expected CapacityExceeded, got {other:?}"),
    }

    manager.remove_client(&ids[0]);

    let (tx4, _rx4) = channel();
    let fourth_id = manager
        .register_client(
            tx4,
            ConnectionCloseSignal::detached(),
            addrs[2],
            Uuid::new_v4(),
        )
        .await
        .expect("registration resumes after a global slot is released");

    // Slot conservation: draining every entry frees the full ceiling.
    manager.remove_client(&ids[1]);
    manager.remove_client(&fourth_id);
    for i in 0..2 {
        let (tx, _rx) = channel();
        manager
            .register_client(
                tx,
                ConnectionCloseSignal::detached(),
                addrs[2],
                Uuid::new_v4(),
            )
            .await
            .unwrap_or_else(|err| {
                panic!("re-admission {i} after full drain must succeed: {err:?}")
            });
    }
}

/// When both caps bind at once, the per-IP refusal wins (the connection
/// manager checks the per-IP budget first), keeping the pre-existing
/// error precedence stable.
#[tokio::test]
async fn per_ip_refusal_wins_when_both_caps_bind() {
    let manager = make_limited_manager(1, 1);
    let addr: SocketAddr = "127.0.0.1:6004".parse().unwrap();
    let (tx1, _rx1) = channel();
    manager
        .register_client(tx1, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("first registration succeeds");

    let (tx2, _rx2) = channel();
    let err = manager
        .register_client(tx2, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect_err("second registration hits both caps");
    assert!(
        matches!(err, RegisterClientError::IpLimitExceeded { .. }),
        "per-IP refusal must take precedence, got {err:?}"
    );
}

/// The test-only registration path bypasses admission (like its per-IP
/// sibling) but still counts through the global counter so that
/// unregistration releases stay balanced: mixed live entries must not
/// wedge the counter above the ceiling or below zero.
#[tokio::test]
async fn test_only_registrations_count_but_bypass_the_global_ceiling() {
    let manager = make_limited_manager(1, 8);
    let addr: SocketAddr = "127.0.0.1:6005".parse().unwrap();

    let (tx1, _rx1) = channel();
    let production_id = manager
        .register_client(tx1, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("production registration consumes the one slot");

    let (tx2, _rx2) = channel();
    let test_id = Uuid::new_v4();
    manager.connect_test_client(test_id, tx2, addr).await;

    manager.remove_client(&production_id);
    manager.remove_client(&test_id);

    let (tx3, _rx3) = channel();
    manager
        .register_client(tx3, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("counter must be fully released after mixed entries drain");
}

/// A same-id test registration replaces the prior entry; the displaced
/// entry's slots (per-IP AND global) are released in place so the
/// counters stay paired with live entries. The replacement arrives from
/// a different address with a per-IP ceiling of 1, so the per-IP release
/// is observed directly: without it, the displaced address's count would
/// stay at 1 and refuse further registrations from that address.
#[tokio::test]
async fn same_id_test_replacement_releases_the_displaced_slots() {
    let manager = make_limited_manager(8, 1);
    let displaced_addr: SocketAddr = "127.0.0.1:6006".parse().unwrap();
    let replacement_addr: SocketAddr = "198.51.100.9:6007".parse().unwrap();
    let reused_id = Uuid::new_v4();

    let (tx1, _rx1) = channel();
    manager
        .connect_test_client(reused_id, tx1, displaced_addr)
        .await;
    let (tx2, _rx2) = channel();
    manager
        .connect_test_client(reused_id, tx2, replacement_addr)
        .await;

    manager.remove_client(&reused_id);

    for (label, addr) in [
        ("displaced", displaced_addr),
        ("replacement", replacement_addr),
    ] {
        let (tx, _rx) = channel();
        manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .unwrap_or_else(|err| {
                panic!("the {label} address must be fully released after the stomp: {err:?}")
            });
    }
}

#[tokio::test]
async fn remove_client_for_unregistration_uses_drain_predicate_at_close_request() {
    let manager = make_manager(1);
    let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();
    let (tx, _rx) = channel();
    let (close, listener) = ConnectionCloseSignal::channel();
    let player_id = manager
        .register_client(tx, close, addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");

    let (_connection, reason) = manager
        .remove_client_for_unregistration(&player_id, || true)
        .expect("client should be removed");

    assert_eq!(reason, crate::coordination::CloseReason::Shutdown);
    assert_eq!(
        listener.requested_reason(),
        Some(crate::coordination::CloseReason::Shutdown),
        "removal must request shutdown when the final drain predicate is true"
    );
}

#[tokio::test]
async fn generic_removal_cannot_discard_a_room_members_terminal_tail() {
    let manager = make_manager(1);
    let addr: SocketAddr = "127.0.0.1:5002".parse().unwrap();
    let room_id = RoomId::from_u128(0x660B_70BA_DA11_4CE1_8168_DA1A_D311_00A0);
    let (tx, _rx) = channel();
    let player_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");
    manager.assign_client_to_room(&player_id, room_id).await;
    assert_eq!(
        manager.next_relay_stamp_in_room(&player_id, &room_id),
        Some(RelayStamp { epoch: 1, seq: 1 })
    );

    assert!(
        manager
            .remove_client_for_unregistration(&player_id, || false)
            .is_none(),
        "generic teardown must leave a room-bound connection intact"
    );
    assert!(manager.has_client(&player_id));
    let (_, terminal_tail) = manager
        .clear_room_assignment_with_tail(&player_id, &room_id)
        .expect("terminal unroute retains the connection and tail");
    assert_eq!(terminal_tail, RelayStamp { epoch: 1, seq: 1 });
    assert!(
        manager
            .remove_client_for_unregistration(&player_id, || false)
            .is_some(),
        "connection removal becomes valid after terminal capture"
    );
}

/// The resumed reconnect epoch is part of the reassignment itself: no
/// provisional value (the transient socket's epoch+1) may ever be visible
/// between reassignment and the caller's epoch resume.
#[tokio::test]
async fn reassign_connection_applies_the_resumed_epoch_immediately() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:5003".parse().unwrap();
    let (tx, _rx) = channel();
    let transient_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");
    let restored_id = PlayerId::new_v4();
    let room_id = RoomId::new_v4();

    manager.reassign_connection(&transient_id, &restored_id, room_id, 7);

    // The very first metadata read through the room-scoped projection API
    // already observes the final incarnation.
    assert_eq!(
        manager.current_relay_stamp_in_room(&restored_id, &room_id),
        Some(RelayStamp { epoch: 7, seq: 0 }),
        "reassignment must publish the resumed epoch atomically"
    );
    // And the next allocated stamp continues that incarnation.
    assert_eq!(
        manager.next_relay_stamp_in_room(&restored_id, &room_id),
        Some(RelayStamp { epoch: 7, seq: 1 }),
    );
}

/// The scrape walks live connections only: a legacy sender contributes
/// nothing, an empty classified queue reads as depth zero with no age,
/// and a resident item shows both its depth and an age measured against
/// the scrape instant. This is the runner's queue_depth/queue_age sample
/// source.
#[tokio::test]
async fn outbound_queue_sample_walks_live_classified_queues_only() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator: Arc<dyn MessageCoordinator> = Arc::new(TestCoordinator::default());
    let manager = Arc::new(ConnectionManager::new(
        usize::MAX,
        4,
        metrics,
        coordinator,
        false,
        (2, tokio::time::Duration::from_secs(60)),
    ));
    let addr: SocketAddr = "127.0.0.1:5045".parse().unwrap();
    let (legacy_tx, _legacy_rx) = tokio::sync::mpsc::channel::<Arc<ServerMessage>>(4);
    let (close_signal, _close_listener) = ConnectionCloseSignal::channel();
    manager
        .register_client(legacy_tx, close_signal.clone(), addr, Uuid::new_v4())
        .await
        .expect("legacy registration succeeds");

    // A legacy-only population reads as an empty sample.
    let sample = manager.outbound_queue_sample();
    assert_eq!(
        sample.total_depth, 0,
        "legacy senders carry no inspectable queue"
    );
    assert_eq!(sample.oldest_enqueued_at, None);
    assert_eq!(sample.oldest_age_millis(tokio::time::Instant::now()), 0);

    let (classified_tx, _classified_rx) = crate::coordination::outbound_queue::channel(4, 4);
    manager
        .register_classified_client(
            DeliverySender::classified(classified_tx.clone()),
            close_signal,
            addr,
            Uuid::new_v4(),
        )
        .await
        .expect("classified registration succeeds");

    // An empty classified queue still reads as empty.
    let sample = manager.outbound_queue_sample();
    assert_eq!(sample.total_depth, 0);
    assert_eq!(sample.oldest_enqueued_at, None);

    // A resident item shows depth and an age that the scrape instant
    // measures (one queued item, then a second one: the OLDEST stamps
    // the age).
    let from_player = PlayerId::new_v4();
    let room_id = RoomId::new_v4();
    let data = |seq: u64| {
        crate::coordination::outbound_queue::OutboundData::new(
            Arc::new(ServerMessage::GameData {
                from_player,
                data: serde_json::json!({ "seq": seq }),
                seq: Some(seq),
                epoch: Some(1),
                class: Some(crate::protocol::DeliveryClass::Reliable),
                key: None,
            }),
            crate::coordination::outbound_queue::DataDeliveryMetadata {
                class: crate::protocol::DeliveryClass::Reliable,
                key: None,
                from_player,
                room_id,
                epoch: 1,
                seq,
            },
        )
    };
    let sender = classified_tx.clone();
    sender
        .try_enqueue_data(data(1))
        .expect("first enqueue fits");
    let (_, first_enqueued_at) = sender.depth_and_oldest();
    let first_enqueued_at = first_enqueued_at.expect("item is resident");
    sender
        .try_enqueue_data(data(2))
        .expect("second enqueue fits");

    let sample = manager.outbound_queue_sample();
    assert_eq!(sample.total_depth, 2, "both resident items are counted");
    assert_eq!(
        sample.oldest_enqueued_at,
        Some(first_enqueued_at),
        "the age stamps the oldest resident item, not the newest"
    );

    // Scrape 250ms later: the age measures exactly the enqueue-to-scrape
    // span, floored at whole milliseconds.
    let age = sample.oldest_age_millis(first_enqueued_at + std::time::Duration::from_millis(250));
    assert_eq!(
        age, 250,
        "age is measured against the passed scrape instant"
    );
}

/// The error-reply budget belongs to the physical socket (issue #518), so
/// a reconnect identity swap must carry its charged state instead of
/// re-arming a fresh budget that a same-socket refuser could multiply.
#[tokio::test]
async fn reassign_connection_carries_the_charged_error_reply_gate() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator: Arc<dyn MessageCoordinator> = Arc::new(TestCoordinator::default());
    let manager = Arc::new(ConnectionManager::new(
        usize::MAX,
        4,
        metrics,
        coordinator,
        false,
        (2, tokio::time::Duration::from_secs(60)),
    ));
    let addr: SocketAddr = "127.0.0.1:5044".parse().unwrap();
    let (tx, _rx) = channel();
    let (close_signal, close_listener) = ConnectionCloseSignal::channel();
    let transient_id = manager
        .register_client(tx, close_signal, addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");

    // Spend one slot on the transient identity (an exhausted budget would
    // pin a transient close and the swap would rightly refuse).
    assert!(manager.charge_error_reply(&transient_id).await);

    let restored_id = PlayerId::new_v4();
    let room_id = RoomId::new_v4();
    assert!(matches!(
        manager.reassign_connection(&transient_id, &restored_id, room_id, 1),
        ReassignmentOutcome::Reassigned(_)
    ));

    // The charged state followed the socket: only one further reply fits,
    // and the next one exhausts. A re-armed fresh gate would pass both.
    assert!(
        manager.charge_error_reply(&restored_id).await,
        "the restored identity resumes the carried budget"
    );
    assert!(
        !manager.charge_error_reply(&restored_id).await,
        "the identity swap must not re-arm the exhausted budget"
    );
    assert_eq!(
        close_listener.requested_reason(),
        Some(crate::coordination::CloseReason::InboundRateLimited),
        "the exhaustion close must be pinned through the swap"
    );
}

/// Mirror of the swap-carry pin for the rollback arm: undoing a reconnect
/// identity swap must hand the charged gate back to the transient
/// identity instead of dropping or re-arming it, so the WebSocket task
/// that keeps using `current_player_id` still answers for the socket's
/// spent budget.
#[tokio::test]
async fn restore_reassigned_connection_carries_the_charged_error_reply_gate_back() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator: Arc<dyn MessageCoordinator> = Arc::new(TestCoordinator::default());
    let manager = Arc::new(ConnectionManager::new(
        usize::MAX,
        4,
        metrics.clone(),
        coordinator,
        false,
        (2, tokio::time::Duration::from_secs(60)),
    ));
    let addr: SocketAddr = "127.0.0.1:5045".parse().unwrap();
    let (tx, _rx) = channel();
    let (close_signal, close_listener) = ConnectionCloseSignal::channel();
    let transient_id = manager
        .register_client(tx, close_signal, addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");

    // Spend one slot on the transient identity.
    assert!(manager.charge_error_reply(&transient_id).await);

    let restored_id = PlayerId::new_v4();
    let room_id = RoomId::new_v4();
    assert!(matches!(
        manager.reassign_connection(&transient_id, &restored_id, room_id, 1),
        ReassignmentOutcome::Reassigned(_)
    ));
    assert!(
        manager
            .restore_reassigned_connection(&transient_id, &restored_id)
            .is_some(),
        "the rollback arm restores the transient identity"
    );

    // The charged state followed the socket back: one reply still fits,
    // and the next one exhausts. A dropped or re-armed gate passes both.
    assert!(
        manager.charge_error_reply(&transient_id).await,
        "the transient identity resumes the carried budget"
    );
    assert!(
        !manager.charge_error_reply(&transient_id).await,
        "the rollback must not re-arm the exhausted budget"
    );
    assert_eq!(
        close_listener.requested_reason(),
        Some(crate::coordination::CloseReason::InboundRateLimited),
        "the exhaustion close must be pinned through the rollback"
    );
    assert_eq!(
        metrics
            .rate_limit_inbound_error_reply_rejections
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the exhaustion metric fires exactly once"
    );
}

/// Close-code attribution under a budget exhaustion that races a
/// competing close: the first pinned reason owns the close frame
/// (`Shutdown` aside), while the exhaustion side effects — rejection
/// metric, farewell, close request — fire exactly once regardless of
/// which order the two conditions land in. A kicked player and a flooder
/// share the same socket, so both orders are real interleavings.
#[tokio::test]
async fn error_reply_exhaustion_arbitrates_with_a_pinned_close_first_reason_wins() {
    for (exhaust_first, expected) in [
        (true, crate::coordination::CloseReason::InboundRateLimited),
        (false, crate::coordination::CloseReason::Kicked),
    ] {
        let metrics = Arc::new(ServerMetrics::new());
        let coordinator: Arc<dyn MessageCoordinator> = Arc::new(TestCoordinator::default());
        let manager = Arc::new(ConnectionManager::new(
            usize::MAX,
            4,
            metrics.clone(),
            coordinator,
            false,
            (1, tokio::time::Duration::from_secs(60)),
        ));
        let addr: SocketAddr = "127.0.0.1:5046".parse().unwrap();
        let (tx, _rx) = channel();
        let (close_signal, close_listener) = ConnectionCloseSignal::channel();
        let player_id = manager
            .register_client(tx, close_signal.clone(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");

        if exhaust_first {
            // The budget spends first: the close request inside
            // `charge_error_reply` pins 4006, and the competing kick
            // cannot overwrite it (its request reports not-set).
            assert!(manager.charge_error_reply(&player_id).await);
            assert!(!manager.charge_error_reply(&player_id).await);
            assert!(!close_signal.request_close(crate::coordination::CloseReason::Kicked));
        } else {
            // The kick pins 4007 first: the exhaustion that follows still
            // fires its metric + farewell + close request, but the close
            // frame keeps the kick's reason.
            assert!(close_signal.request_close(crate::coordination::CloseReason::Kicked));
            assert!(manager.charge_error_reply(&player_id).await);
            assert!(!manager.charge_error_reply(&player_id).await);
        }

        assert_eq!(
            close_listener.requested_reason(),
            Some(expected),
            "the first pinned close reason must win the frame"
        );
        // One-shot: further charges in the same spent window must not
        // re-fire the metric or flip the pinned reason.
        assert!(!manager.charge_error_reply(&player_id).await);
        assert_eq!(
            close_listener.requested_reason(),
            Some(expected),
            "repeat exhaustions must not overwrite the pinned close"
        );
        assert_eq!(
            metrics
                .rate_limit_inbound_error_reply_rejections
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the exhaustion metric fires exactly once per window in both orders"
        );
    }
}

/// A reconnect identity swap landing inside `charge_error_reply`'s
/// farewell await must not skip the `4006` close (#697): the close request
/// pins the captured per-socket signal, not a fresh map lookup under the
/// transient key. The one-shot `report_exhaustion` never retries a skipped
/// pin, so a key-lookup pin would leave the exhausted socket open until
/// the window rolls over and exhausts again.
#[tokio::test]
async fn error_reply_exhaustion_pins_the_close_through_a_rekey_inside_the_farewell_await() {
    let metrics = Arc::new(ServerMetrics::new());
    let (gate, mut farewell_entered, farewell_release) = FarewellGate::channel();
    let coordinator: Arc<dyn MessageCoordinator> = Arc::new(TestCoordinator {
        farewell_gate: Some(Arc::new(gate)),
        ..TestCoordinator::default()
    });
    let manager = Arc::new(ConnectionManager::new(
        usize::MAX,
        4,
        metrics.clone(),
        coordinator,
        false,
        (1, tokio::time::Duration::from_secs(60)),
    ));
    let addr: SocketAddr = "127.0.0.1:5048".parse().unwrap();
    let (tx, _rx) = channel();
    let (close_signal, close_listener) = ConnectionCloseSignal::channel();
    let transient_id = manager
        .register_client(tx, close_signal, addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");

    // Spend the window's single reply so the next charge is the
    // exhaustion whose side effects include the farewell await.
    assert!(manager.charge_error_reply(&transient_id).await);

    // Exhaust inside a spawned task so the test can interleave the rekey
    // while the farewell await is parked.
    let exhaustion = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move { manager.charge_error_reply(&transient_id).await }
    });
    farewell_entered.recv().await.expect("farewell is reached");

    // Land the swap inside that await: the entry moves from the transient
    // key to the restored key, carrying the same physical-socket signal.
    let restored_id = PlayerId::new_v4();
    let room_id = RoomId::new_v4();
    assert!(matches!(
        manager.reassign_connection(&transient_id, &restored_id, room_id, 1),
        ReassignmentOutcome::Reassigned(_)
    ));

    let _ = farewell_release.send(());
    assert!(
        !exhaustion.await.expect("exhaustion task completes"),
        "the exhausting charge must still refuse the reply"
    );
    assert_eq!(
        close_listener.requested_reason(),
        Some(crate::coordination::CloseReason::InboundRateLimited),
        "the 4006 close must pin the restored connection through the swap"
    );

    // One-shot accounting is unchanged: a later exhausted charge under the
    // restored identity neither re-fires the metric nor flips the pin.
    assert!(
        !manager.charge_error_reply(&restored_id).await,
        "the restored identity resumes the exhausted budget"
    );
    assert_eq!(
        close_listener.requested_reason(),
        Some(crate::coordination::CloseReason::InboundRateLimited),
        "repeat exhaustions must not overwrite the pinned close"
    );
    assert_eq!(
        metrics
            .rate_limit_inbound_error_reply_rejections
            .load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the exhaustion metric fires exactly once"
    );
}

/// A stale terminal unroute for a room the player no longer (or never did)
/// belongs to must be refused untouched — publishing the current
/// assignment's live stamp as a foreign room's terminal watermark would
/// fabricate a phantom `PlayerLeft` tail and sever live membership.
#[tokio::test]
async fn clear_room_assignment_with_tail_refuses_a_foreign_room() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:5004".parse().unwrap();
    let (tx, _rx) = channel();
    let player_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");
    let room_a = RoomId::new_v4();
    let room_b = RoomId::new_v4();
    manager.assign_client_to_room(&player_id, room_a).await;
    assert_eq!(
        manager.next_relay_stamp_in_room(&player_id, &room_a),
        Some(RelayStamp { epoch: 1, seq: 1 })
    );

    // Wrong-room capture: refused, nothing consumed or cleared.
    assert!(
        manager
            .clear_room_assignment_with_tail(&player_id, &room_b)
            .is_none(),
        "a foreign expected_room must not capture a tail"
    );
    assert_eq!(
        manager.current_relay_stamp_in_room(&player_id, &room_a),
        Some(RelayStamp { epoch: 1, seq: 1 }),
        "membership and stamp stream stay intact after the refusal"
    );

    // Correct-room capture still works exactly as before.
    let (_, tail) = manager
        .clear_room_assignment_with_tail(&player_id, &room_a)
        .expect("matching expected_room captures the terminal tail");
    assert_eq!(tail, RelayStamp { epoch: 1, seq: 1 });
}

/// GAP-3 regression: a 16-player session behind a single NAT must be
/// admissible at the DEFAULT per-IP cap. Before A3 the default was 10, so
/// the 11th same-IP client was refused. Builds a manager at the real
/// `default_max_connections_per_ip()` and registers 16 clients from one IP.
#[tokio::test]
async fn default_ip_cap_admits_a_sixteen_player_nat() {
    let cap = crate::config::defaults::default_max_connections_per_ip();
    assert!(
        cap >= 16,
        "default per-IP cap ({cap}) must admit a 16-player NAT session"
    );

    let manager = make_manager(cap);
    let mut ids = Vec::new();
    for i in 0..16u16 {
        let (tx, _rx) = channel();
        let addr: SocketAddr = format!("203.0.113.7:{}", 6000 + i).parse().unwrap();
        let id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .unwrap_or_else(|e| panic!("client {i} from one IP must be admitted: {e:?}"));
        ids.push(id);
    }
    assert_eq!(
        ids.len(),
        16,
        "all 16 same-IP clients admitted at default cap"
    );
}

/// The heartbeat-update throttle (`should_update_last_seen`) is deterministic
/// under the paused-clock runtime: the first observation always updates, then
/// updates are suppressed until the threshold has *elapsed* on the runtime
/// clock. Driven purely by `tokio::time::advance(..)` — no wall-clock sleep,
/// so nothing can flake under load (this is the B4 payoff: the reaper /
/// throttle windows read `tokio::time::Instant`).
#[tokio::test(start_paused = true)]
async fn should_update_last_seen_throttles_until_threshold_elapses() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:7100".parse().unwrap();
    let (tx, _rx) = channel();
    let player_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");

    let threshold = std::time::Duration::from_secs(30);

    // First observation always updates (no prior timestamp recorded).
    assert!(
        manager.should_update_last_seen(&player_id, threshold),
        "first observation must update"
    );
    // Immediately after, the throttle suppresses another update.
    assert!(
        !manager.should_update_last_seen(&player_id, threshold),
        "an update within the throttle window must be suppressed"
    );

    // Just below the threshold: still suppressed (the boundary is `>=`).
    tokio::time::advance(threshold - std::time::Duration::from_millis(1)).await;
    assert!(
        !manager.should_update_last_seen(&player_id, threshold),
        "an update just under the threshold must stay suppressed"
    );

    // Crossing the threshold releases exactly one update, then re-throttles
    // from the new baseline.
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert!(
        manager.should_update_last_seen(&player_id, threshold),
        "an update at/after the threshold must fire"
    );
    assert!(
        !manager.should_update_last_seen(&player_id, threshold),
        "the update after the release is throttled from the new baseline"
    );
}

/// A missing player must be throttled OFF, not waved through: an unregistered
/// id can never take a throttle stamp, so "allow" would fire the metric
/// increment and persistence attempt on every in-flight frame racing its
/// teardown — violating the once-per-player-per-window invariant. A player
/// registered afterwards starts with a clean throttle baseline.
/// Clock-independent (the not-found branch never reads the clock).
#[tokio::test]
async fn should_update_last_seen_suppresses_unknown_player() {
    let manager = make_manager(4);
    let unknown = Uuid::new_v4();
    assert!(
        !manager.should_update_last_seen(&unknown, std::time::Duration::from_secs(30)),
        "unknown player must be suppressed, not allowed"
    );

    // The suppression is not sticky state: a player registered after the
    // refused lookups still observes a normal first-update release.
    let addr: SocketAddr = "127.0.0.1:7101".parse().unwrap();
    let (tx, _rx) = channel();
    let late_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");
    assert!(
        manager.should_update_last_seen(&late_id, std::time::Duration::from_secs(30)),
        "freshly registered player keeps the first-update release"
    );
}

/// The activity reaper owns the client-facing silent-disconnect decision,
/// and its two seams must flip at the same boundary: a client silent for
/// exactly `ping_timeout` is still alive (the snapshot does not collect it
/// and the atomic revalidation refuses to pin the close), while the same
/// silent client is expired one tick later. Driven purely by
/// `tokio::time::advance(..)` so "exactly at" is deterministic; any drift
/// between the snapshot and the pin-before-teardown revalidation (the
/// pairing that lets a racing Pong rescue the connection) fails here.
#[tokio::test(start_paused = true)]
async fn activity_reaper_expiry_flips_once_at_the_ping_timeout_boundary() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:7102".parse().unwrap();
    let (tx, _rx) = channel();
    let (close, listener) = ConnectionCloseSignal::channel();
    let player_id = manager
        .register_client(tx, close, addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");
    let ping_timeout = std::time::Duration::from_millis(50);

    // Exactly at the timeout the client is still alive on both seams.
    tokio::time::advance(ping_timeout).await;
    assert!(
        manager.collect_expired_clients(ping_timeout).is_empty(),
        "a client silent for exactly ping_timeout must survive the snapshot"
    );
    assert!(
        !manager.request_activity_timeout_if_expired(&player_id, ping_timeout),
        "a client silent for exactly ping_timeout must not be pinned"
    );
    assert_eq!(
        listener.requested_reason(),
        None,
        "the boundary-spared client must keep a clean close state"
    );

    // One tick later the same silent client is expired on both seams, and
    // the revalidation pins the client-facing ActivityTimeout close.
    tokio::time::advance(std::time::Duration::from_nanos(1)).await;
    assert_eq!(
        manager.collect_expired_clients(ping_timeout),
        vec![player_id],
        "a client silent past ping_timeout must appear in the snapshot"
    );
    assert!(
        manager.request_activity_timeout_if_expired(&player_id, ping_timeout),
        "a client silent past ping_timeout must be pinned for teardown"
    );
    assert_eq!(
        listener.requested_reason(),
        Some(CloseReason::ActivityTimeout),
        "the pinned close must carry the activity-timeout reason"
    );
}

#[tokio::test]
async fn assign_client_to_room_updates_coordinator_membership() {
    let metrics = Arc::new(ServerMetrics::new());
    let coordinator = Arc::new(TestCoordinator::default());
    let manager = Arc::new(ConnectionManager::new(
        usize::MAX,
        4,
        metrics.clone(),
        coordinator.clone() as Arc<dyn MessageCoordinator>,
        false,
        (u32::MAX, tokio::time::Duration::from_secs(60)),
    ));

    let (tx, _rx) = channel();
    let addr: SocketAddr = "127.0.0.1:6000".parse().unwrap();
    let player_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");

    let room_id = RoomId::new_v4();
    manager.assign_client_to_room(&player_id, room_id).await;

    assert_eq!(manager.get_client_room(&player_id), Some(room_id));

    let registrations = coordinator.registrations.lock().await;
    assert_eq!(registrations.len(), 2);
    assert_eq!(registrations[0], (player_id, None));
    assert_eq!(registrations[1], (player_id, Some(room_id)));
}

#[tokio::test]
async fn relay_stamp_allocation_is_bound_to_the_expected_room() {
    let manager = make_manager(4);
    let (tx, _rx) = channel();
    let addr: SocketAddr = "127.0.0.1:6001".parse().unwrap();
    let player_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration succeeds");
    let room_a = RoomId::from_u128(0xAAAA);
    let room_b = RoomId::from_u128(0xBBBB);

    manager.assign_client_to_room(&player_id, room_a).await;
    assert_eq!(
        manager.next_relay_stamp_in_room(&player_id, &room_a),
        Some(RelayStamp { epoch: 1, seq: 1 })
    );
    assert_eq!(
        manager.current_relay_stamp_in_room(&player_id, &room_a),
        Some(RelayStamp { epoch: 1, seq: 1 })
    );

    manager.clear_room_assignment(&player_id);
    assert_eq!(
        manager.next_relay_stamp_in_room(&player_id, &room_a),
        None,
        "roomless teardown window must cancel the old-room relay"
    );
    assert_eq!(
        manager.current_relay_stamp_in_room(&player_id, &room_a),
        None,
        "roomless players must be filtered from live snapshots"
    );

    manager.assign_client_to_room(&player_id, room_b).await;
    assert_eq!(
        manager.next_relay_stamp_in_room(&player_id, &room_a),
        None,
        "a room switch must not allocate against the stale room"
    );
    assert_eq!(
        manager.next_relay_stamp_in_room(&player_id, &room_b),
        Some(RelayStamp { epoch: 2, seq: 1 })
    );
}

// -----------------------------------------------------------------------
// D. Thread safety tests for ConnectionManager
// -----------------------------------------------------------------------

/// D17: Many clients from the same IP; verify counter accuracy.
///
/// max_connections_per_ip = 5.
/// 20 tasks concurrently try to register from the same IP.
/// Exactly 5 should succeed.
/// After removing all 5, the counter should be back to 0.
#[tokio::test]
async fn test_concurrent_ip_slot_reservation() {
    let manager = make_manager(5);
    let addr: SocketAddr = "10.0.0.1:9000".parse().unwrap();

    let task_count = 20;
    let barrier = Arc::new(tokio::sync::Barrier::new(task_count));
    let manager = Arc::new(manager);
    let mut handles = Vec::with_capacity(task_count);

    for _ in 0..task_count {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let (tx, _rx) = channel();
            manager
                .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
                .await
        }));
    }

    let mut successes = Vec::new();
    let mut failures = 0usize;
    for handle in handles {
        match handle.await.expect("task should not panic") {
            Ok(player_id) => successes.push(player_id),
            Err(_) => failures += 1,
        }
    }

    assert_eq!(
        successes.len(),
        5,
        "Exactly 5 should succeed, got {}",
        successes.len()
    );
    assert_eq!(failures, 15, "15 should be rejected, got {failures}");

    // Remove all 5 successful clients
    for pid in &successes {
        manager.remove_client(pid);
    }

    // After removal, new registrations should work (counter is back to 0)
    let (tx, _rx) = channel();
    let result = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await;
    assert!(
        result.is_ok(),
        "Registration should succeed after all clients removed"
    );
}

/// D18: Reassignment does not leak IP slots.
///
/// The swap refuses when a live entry already occupies the target id
/// instead of silently stomping it, and both entries stay untouched — the
/// same defense the rollback sibling (`restore_reassigned_connection`)
/// applies with its `contains_key` check.
#[tokio::test]
async fn reassign_connection_refuses_an_occupied_target_instead_of_stomping_it() {
    let manager = make_manager(8);
    let occupant_id = Uuid::new_v4();
    let (tx, _rx) = channel();
    manager
        .connect_test_client(occupant_id, tx, "10.0.0.64:9000".parse().unwrap())
        .await;

    let (transient_tx, _transient_rx) = channel();
    let transient_id = manager
        .register_client(
            transient_tx,
            ConnectionCloseSignal::detached(),
            "10.0.0.65:9000".parse().unwrap(),
            Uuid::new_v4(),
        )
        .await
        .expect("transient registration succeeds");

    let outcome = manager.reassign_connection(&transient_id, &occupant_id, RoomId::new_v4(), 1);
    assert!(
        matches!(outcome, ReassignmentOutcome::RefusedTargetOccupied),
        "an occupied target must be refused, got {outcome:?}"
    );
    assert!(
        manager.has_client(&occupant_id),
        "the occupant entry must survive the refused swap"
    );
    assert!(
        manager.has_client(&transient_id),
        "the transient entry must survive the refused swap"
    );
}

/// Register a client, reassign to a new player_id.
/// IP count should still be 1 (not 0 or 2).
/// Verify by filling up to the per-IP limit, then remove the reassigned
/// client and confirm the freed slot allows a new registration.
#[tokio::test]
async fn test_reassign_connection_preserves_ip_count() {
    let manager = make_manager(5);
    let addr: SocketAddr = "10.0.0.2:9000".parse().unwrap();

    let (tx, _rx) = channel();
    let original_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration should succeed");

    let room_id = RoomId::new_v4();
    let new_player_id = Uuid::new_v4();

    let reassigned = manager.reassign_connection(&original_id, &new_player_id, room_id, 1);
    assert!(
        matches!(reassigned, ReassignmentOutcome::Reassigned(_)),
        "Reassignment should succeed"
    );

    // Original player should be gone
    assert!(
        !manager.has_client(&original_id),
        "Original player should no longer exist"
    );
    assert!(
        manager.has_client(&new_player_id),
        "New player should exist"
    );

    // IP slot should still be 1 (not 0 or 2)
    // Verify by trying to register 4 more (max is 5, 1 already used)
    for i in 0..4 {
        let (tx, _rx) = channel();
        let port = 9001 + i;
        let new_addr: SocketAddr = format!("10.0.0.2:{port}").parse().unwrap();
        manager
            .register_client(
                tx,
                ConnectionCloseSignal::detached(),
                new_addr,
                Uuid::new_v4(),
            )
            .await
            .expect("should succeed within limit");
    }

    // 5th attempt from same IP should fail (already at limit)
    let (tx, _rx) = channel();
    let new_addr: SocketAddr = "10.0.0.2:10000".parse().unwrap();
    let result = manager
        .register_client(
            tx,
            ConnectionCloseSignal::detached(),
            new_addr,
            Uuid::new_v4(),
        )
        .await;
    assert!(
        result.is_err(),
        "6th connection from same IP should be rejected"
    );

    // Remove the reassigned client and verify IP slot is freed
    manager.remove_client(&new_player_id);
    assert!(
        !manager.has_client(&new_player_id),
        "Client should be removed"
    );

    // After removing the reassigned client, the slot should be freed.
    // Verify by registering one more from the same IP (was at limit before removal).
    let (tx_verify, _rx_verify) = channel();
    let verify_addr: SocketAddr = "10.0.0.2:10001".parse().unwrap();
    let result = manager
        .register_client(
            tx_verify,
            ConnectionCloseSignal::detached(),
            verify_addr,
            Uuid::new_v4(),
        )
        .await;
    assert!(
        result.is_ok(),
        "Registration should succeed after removing the reassigned client"
    );
}

#[tokio::test]
async fn reassign_and_restore_preserve_physical_lifecycle_identity() {
    let manager = make_manager(5);
    let addr: SocketAddr = "10.0.0.20:9000".parse().unwrap();
    let (tx, _rx) = channel();
    let transient_id = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("registration should succeed");
    let restored_id = PlayerId::new_v4();
    let lifecycle = manager
        .client_lifecycle(&transient_id)
        .expect("registered connection has lifecycle identity");

    expect_reassigned(manager.reassign_connection(
        &transient_id,
        &restored_id,
        RoomId::new_v4(),
        1,
    ));
    let reassigned_lifecycle = manager
        .client_lifecycle(&restored_id)
        .expect("restored id owns lifecycle identity");
    assert!(Arc::ptr_eq(&lifecycle, &reassigned_lifecycle));
    assert_eq!(lifecycle.player_id(), restored_id);
    assert!(manager.lifecycle_matches(&restored_id, &lifecycle));
    assert!(!manager.lifecycle_matches(&transient_id, &lifecycle));

    manager
        .restore_reassigned_connection(&transient_id, &restored_id)
        .expect("rollback succeeds");
    let rolled_back_lifecycle = manager
        .client_lifecycle(&transient_id)
        .expect("transient id regains lifecycle identity");
    assert!(Arc::ptr_eq(&lifecycle, &rolled_back_lifecycle));
    assert_eq!(lifecycle.player_id(), transient_id);
    assert!(manager.lifecycle_matches(&transient_id, &lifecycle));
    assert!(!manager.lifecycle_matches(&restored_id, &lifecycle));
}

/// A per-socket close pinned on the transient entry must refuse the
/// identity swap and restore the entry untouched: the pending close may
/// tear down only the transient socket, never the restored connection.
#[tokio::test]
async fn reassign_refused_while_transient_socket_carries_per_socket_close() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:5104".parse().unwrap();
    let restored_id = PlayerId::new_v4();
    let room_id = RoomId::new_v4();

    for reason in [
        crate::coordination::CloseReason::ActivityTimeout,
        crate::coordination::CloseReason::IdleTimeout,
        crate::coordination::CloseReason::SlowConsumer,
        crate::coordination::CloseReason::AuthTimeout,
        crate::coordination::CloseReason::OutboundMessageTooLarge,
        crate::coordination::CloseReason::Unregistered,
    ] {
        let (tx, _rx) = channel();
        let transient_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");
        let pinned = manager.request_close_for(&transient_id, reason);
        assert!(
            pinned,
            "{reason:?}: pin must be the first close on the entry"
        );
        match manager.reassign_connection(&transient_id, &restored_id, room_id, 1) {
            ReassignmentOutcome::RefusedTransientClose(pinned) => {
                assert_eq!(pinned, reason);
            }
            other => panic!("{reason:?}: expected refusal, got {other:?}"),
        }
        // The transient entry must be back, keyed by its own id, still the
        // only registration — the eviction proceeds against it.
        assert!(
            manager.has_client(&transient_id),
            "{reason:?}: refusal must restore the transient entry"
        );
        assert!(
            !manager.has_client(&restored_id),
            "{reason:?}: refusal must not publish the restored identity"
        );
        manager.remove_client(&transient_id);
    }
}

/// Identity/room-scoped closes still cross the swap: a drain must close
/// restored connections, and a room-inactive pin reflects the room the
/// claim just verified.
#[tokio::test]
async fn reassign_still_adopts_entry_pinned_for_shutdown() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:5105".parse().unwrap();
    for reason in [
        crate::coordination::CloseReason::Shutdown,
        crate::coordination::CloseReason::RoomInactive,
    ] {
        let (tx, _rx) = channel();
        let transient_id = manager
            .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
            .await
            .expect("registration succeeds");
        assert!(manager.request_close_for(&transient_id, reason));
        expect_reassigned(manager.reassign_connection(
            &transient_id,
            &PlayerId::new_v4(),
            RoomId::new_v4(),
            1,
        ));
    }
}

// -----------------------------------------------------------------------
// Protocol capability negotiation (P1).
// -----------------------------------------------------------------------

#[tokio::test]
async fn protocol_defaults_to_v2_relay_only() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:7100".parse().unwrap();
    let (tx, _rx) = channel();
    let pid = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("register");

    let proto = manager.protocol(&pid);
    assert_eq!(proto.version, 2);
    assert_eq!(proto.transports, vec![Transport::Relay]);
    assert_eq!(proto.topologies, vec![Topology::Relay]);

    assert!(!manager.supports_v3(&pid));
    assert!(manager.supports_transport(&pid, Transport::Relay));
    assert!(!manager.supports_transport(&pid, Transport::WebRtc));
}

#[tokio::test]
async fn set_protocol_updates_capabilities_and_v3_gate() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:7101".parse().unwrap();
    let (tx, _rx) = channel();
    let pid = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("register");

    manager.set_protocol(
        &pid,
        NegotiatedProtocol {
            version: 3,
            transports: vec![Transport::Relay, Transport::WebRtc],
            topologies: vec![Topology::Relay, Topology::Mesh],
        },
    );

    assert!(manager.supports_v3(&pid));
    assert!(manager.supports_transport(&pid, Transport::WebRtc));
    assert!(manager.supports_transport(&pid, Transport::Relay));
    assert!(!manager.supports_transport(&pid, Transport::Direct));

    let proto = manager.protocol(&pid);
    assert_eq!(proto.version, 3);
    assert_eq!(proto.topologies, vec![Topology::Relay, Topology::Mesh]);
}

#[tokio::test]
async fn protocol_helpers_default_for_unknown_player() {
    let manager = make_manager(4);
    let unknown = Uuid::new_v4();
    // Unknown player => default (v2 relay-only), not v3.
    let proto = manager.protocol(&unknown);
    assert_eq!(proto.version, 2);
    assert!(!manager.supports_v3(&unknown));
    assert!(!manager.supports_transport(&unknown, Transport::Relay));
}

#[tokio::test]
async fn protocol_is_preserved_across_reconnect() {
    let manager = make_manager(4);
    let addr: SocketAddr = "127.0.0.1:7102".parse().unwrap();
    let (tx, _rx) = channel();
    let original = manager
        .register_client(tx, ConnectionCloseSignal::detached(), addr, Uuid::new_v4())
        .await
        .expect("register");

    manager.set_protocol(
        &original,
        NegotiatedProtocol {
            version: 3,
            transports: vec![Transport::Relay, Transport::WebRtc],
            topologies: vec![Topology::Relay, Topology::Mesh],
        },
    );

    let new_pid = Uuid::new_v4();
    let room = RoomId::new_v4();
    assert!(matches!(
        manager.reassign_connection(&original, &new_pid, room, 1),
        ReassignmentOutcome::Reassigned(_)
    ));

    // The migrated connection keeps its negotiated v3 capabilities.
    let proto = manager.protocol(&new_pid);
    assert_eq!(proto.version, 3);
    assert!(manager.supports_v3(&new_pid));
    assert!(manager.supports_transport(&new_pid, Transport::WebRtc));
}

/// D19: Multiple concurrent releases do not underflow the IP counter.
///
/// Register 3 clients from the same IP.
/// Concurrently remove all 3.
/// After removal, new registrations should work (no underflow).
#[tokio::test]
async fn test_concurrent_release_ip_slot_no_underflow() {
    let manager = make_manager(10);

    // Register 3 clients from same IP (different ports for each)
    let mut player_ids = Vec::new();
    for i in 0..3u16 {
        let (tx, _rx) = channel();
        let port_addr: SocketAddr = format!("10.0.0.3:{}", 9000 + i).parse().unwrap();
        let pid = manager
            .register_client(
                tx,
                ConnectionCloseSignal::detached(),
                port_addr,
                Uuid::new_v4(),
            )
            .await
            .expect("registration should succeed");
        player_ids.push(pid);
    }

    // Concurrently remove all 3
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for pid in player_ids {
        let manager = Arc::clone(&manager);
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            manager.remove_client(&pid);
        }));
    }

    for handle in handles {
        handle.await.expect("task should not panic");
    }

    // After all removals, IP should be completely cleared.
    // Verify by registering up to max_connections_per_ip (10).
    for i in 0..10u16 {
        let (tx, _rx) = channel();
        let port_addr: SocketAddr = format!("10.0.0.3:{}", 8000 + i).parse().unwrap();
        let result = manager
            .register_client(
                tx,
                ConnectionCloseSignal::detached(),
                port_addr,
                Uuid::new_v4(),
            )
            .await;
        assert!(
            result.is_ok(),
            "Registration #{} should succeed after complete removal (no underflow)",
            i + 1
        );
    }
}
