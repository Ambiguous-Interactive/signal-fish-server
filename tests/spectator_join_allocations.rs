//! Allocation contract for the complete spectator admission path.

mod test_helpers;

mod allocation_harness;

use signal_fish_server::protocol::{ServerMessage, SpectatorInfo};
use uuid::Uuid;

#[global_allocator]
static ALLOCATOR: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

/// Bound roster-copy growth in isolated runtimes with warmed scheduler storage.
fn spectator_admission_reuses_its_owned_room_and_roster() {
    #[cfg(feature = "allocation-tracking")]
    signal_fish_server::server::allocation_benchmark::install_spectator_allocation_observer(
        record_stage,
    )
    .unwrap();
    let mut profiles = Vec::new();
    for spectators in [0_u128, 16, 254] {
        let runtime = tokio::runtime::Builder::new_current_thread()
            // Poll both scheduler queues and the driver during warm-up. Their
            // native locks must not first initialize inside the measured join.
            .global_queue_interval(1)
            .event_interval(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            allocation_harness::warm_current_thread_runtime().await;
            let server = test_helpers::create_test_server().await;
            let host = Uuid::from_u128(1);
            let spectator = Uuid::from_u128(2);
            let (host_sender, mut host_receiver) = tokio::sync::mpsc::channel(16);
            let (spectator_sender, mut spectator_receiver) = tokio::sync::mpsc::channel(16);
            server.connect_client(host, host_sender).await;
            server.connect_client(spectator, spectator_sender).await;
            server
                .handle_join_room(
                    &host,
                    "spectator-profile".into(),
                    Some("SPROF1".into()),
                    "Host".into(),
                    Some(4),
                    Some(true),
                    None,
                    None,
                    None,
                )
                .await;
            let room_joined = host_receiver.try_recv().unwrap();
            assert!(matches!(room_joined.as_ref(), ServerMessage::RoomJoined(_)));
            let lobby = host_receiver.try_recv().unwrap();
            assert!(matches!(
                lobby.as_ref(),
                ServerMessage::LobbyStateChanged { .. }
            ));
            let room_id = server.get_client_room(&host).await.unwrap();
            server
                .database()
                .set_room_max_spectators(&room_id, Some(255))
                .await
                .unwrap();
            for index in 0..spectators {
                assert!(server
                    .database()
                    .add_spectator_to_room(
                        &room_id,
                        SpectatorInfo {
                            id: Uuid::from_u128(index + 3),
                            name: format!("Viewer{index}"),
                            connected_at: None,
                        },
                    )
                    .await
                    .unwrap());
            }
            server
                .handle_join_as_spectator(
                    &spectator,
                    "spectator-profile".into(),
                    "SPROF1".into(),
                    "Viewer".into(),
                    None,
                )
                .await;
            let warm_baseline = spectator_receiver.try_recv().unwrap();
            assert!(matches!(
                warm_baseline.as_ref(),
                ServerMessage::SpectatorJoined(_)
            ));
            let warm_notification = host_receiver.try_recv().unwrap();
            assert!(matches!(
                warm_notification.as_ref(),
                ServerMessage::NewSpectatorJoined { .. }
            ));
            server.handle_leave_spectator(&spectator).await;
            let left = spectator_receiver.try_recv().unwrap();
            assert!(matches!(left.as_ref(), ServerMessage::SpectatorLeft { .. }));
            let disconnected = host_receiver.try_recv().unwrap();
            assert!(matches!(
                disconnected.as_ref(),
                ServerMessage::SpectatorDisconnected { .. }
            ));

            let game_name = "spectator-profile".into();
            let room_code = "SPROF1".into();
            let spectator_name = "Viewer".into();
            let region = stats_alloc::Region::new(ALLOCATOR);
            #[cfg(feature = "allocation-tracking")]
            {
                RECORDING.store(true, std::sync::atomic::Ordering::Relaxed);
                record_stage("before_call");
            }
            server
                .handle_join_as_spectator(&spectator, game_name, room_code, spectator_name, None)
                .await;
            #[cfg(feature = "allocation-tracking")]
            {
                record_stage("returned");
                RECORDING.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            let stats = region.change();
            #[cfg(feature = "allocation-tracking")]
            print_stages(spectators);
            println!("SPECTATOR_JOIN_PROFILE spectators={spectators} stats={stats:?}");
            profiles.push((spectators, stats.allocations));

            let baseline = spectator_receiver.try_recv().unwrap();
            let ServerMessage::SpectatorJoined(payload) = baseline.as_ref() else {
                panic!("expected spectator baseline, got {baseline:?}");
            };
            assert_eq!(payload.room_id, room_id);
            assert_eq!(payload.room_code, "SPROF1");
            assert_eq!(payload.game_name, "spectator-profile");
            assert_eq!(payload.current_players.len(), 1);
            assert_eq!(payload.current_players[0].id, host);
            assert_eq!(
                payload.current_spectators.len(),
                usize::try_from(spectators).unwrap() + 1
            );
            for index in 0..spectators {
                let expected_id = Uuid::from_u128(index + 3);
                let expected_name = format!("Viewer{index}");
                assert!(
                    payload.current_spectators.iter().any(|entry| {
                        entry.id == expected_id
                            && entry.name == expected_name
                            && entry.connected_at.is_none()
                    }),
                    "baseline is missing spectator {expected_id}"
                );
            }
            let admitted = payload
                .current_spectators
                .iter()
                .find(|entry| entry.id == spectator)
                .expect("baseline includes the admitted spectator");
            assert_eq!(admitted.name, "Viewer");
            assert!(admitted.connected_at.is_some());
            let notification = host_receiver.try_recv().unwrap();
            let ServerMessage::NewSpectatorJoined {
                spectator: joined,
                current_spectators,
                spectator_count,
                ..
            } = notification.as_ref()
            else {
                panic!("expected spectator notification, got {notification:?}");
            };
            assert_eq!(joined.id, spectator);
            assert_eq!(
                serde_json::to_value(joined).unwrap(),
                serde_json::to_value(admitted).unwrap()
            );
            assert_eq!(
                serde_json::to_value(current_spectators).unwrap(),
                serde_json::to_value(&payload.current_spectators).unwrap()
            );
            assert_eq!(
                *spectator_count,
                Some(u32::try_from(spectators).unwrap() + 1)
            );
        });
    }
    println!("SPECTATOR_JOIN_ALLOCATIONS {profiles:?}");
    let empty_allocations = profiles[0].1;
    for (spectators, allocations) in profiles {
        assert!(
            allocations <= empty_allocations + 3 * usize::try_from(spectators).unwrap(),
            "spectator roster allocation growth exceeded three copies: spectators={spectators} allocations={allocations} empty={empty_allocations}"
        );
    }
}

#[cfg(feature = "allocation-tracking")]
const STAGES: [&str; 20] = [
    "before_call",
    "before_spawn",
    "task_prepared",
    "task_started",
    "lifecycle_found",
    "lifecycle_locked",
    "owned_start",
    "code_lock",
    "room_read",
    "policy_checks",
    "insert",
    "generation",
    "baseline_read",
    "readiness",
    "roster",
    "registration",
    "registered",
    "notification",
    "completed",
    "returned",
];
#[cfg(feature = "allocation-tracking")]
static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "allocation-tracking")]
static ALLOCATIONS: [std::sync::atomic::AtomicUsize; 20] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 20];
#[cfg(feature = "allocation-tracking")]
static BYTES: [std::sync::atomic::AtomicUsize; 20] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 20];

/// Record cumulative allocator counters without heap storage or native locks.
#[cfg(feature = "allocation-tracking")]
fn record_stage(stage: &'static str) {
    use std::sync::atomic::Ordering::Relaxed;
    if !RECORDING.load(Relaxed) {
        return;
    }
    let index = STAGES
        .iter()
        .position(|candidate| *candidate == stage)
        .unwrap();
    let stats = ALLOCATOR.stats();
    ALLOCATIONS[index].store(stats.allocations, Relaxed);
    BYTES[index].store(stats.bytes_allocated, Relaxed);
}

/// Print stage deltas only after the complete admission measurement ends.
#[cfg(feature = "allocation-tracking")]
fn print_stages(spectators: u128) {
    use std::sync::atomic::Ordering::Relaxed;
    for index in 1..STAGES.len() {
        let allocations = ALLOCATIONS[index].load(Relaxed) - ALLOCATIONS[index - 1].load(Relaxed);
        let bytes = BYTES[index].load(Relaxed) - BYTES[index - 1].load(Relaxed);
        println!("ADMISSION_STAGE spectators={spectators} stage={} allocations={allocations} bytes={bytes}", STAGES[index]);
    }
}

/// Bare task admission separates lazy native registry locks from room work.
fn runtime_task_registry_initializes_each_shard_once() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::task::yield_now().await;
        let mut profiles = Vec::with_capacity(8);
        for _ in 0..8 {
            let region = stats_alloc::Region::new(ALLOCATOR);
            tokio::spawn(std::future::ready(())).await.unwrap();
            profiles.push(region.change());
        }
        println!("TASK_REGISTRY profiles={profiles:?}");
        for index in 0..4 {
            let cold = profiles[index];
            let warm = profiles[index + 4];
            #[cfg(target_os = "macos")]
            {
                assert_eq!(cold.allocations, warm.allocations + 1, "{profiles:?}");
                assert_eq!(
                    cold.bytes_allocated,
                    warm.bytes_allocated + 64,
                    "{profiles:?}"
                );
            }
            #[cfg(not(target_os = "macos"))]
            {
                assert_eq!(cold.allocations, warm.allocations, "{profiles:?}");
                assert_eq!(cold.bytes_allocated, warm.bytes_allocated, "{profiles:?}");
            }
            assert_eq!(warm.allocations, 1, "{profiles:?}");
            assert_eq!(warm.reallocations, 0, "{profiles:?}");
        }
    });
}

/// Run allocation histories without a concurrent test-runner thread.
fn main() {
    allocation_harness::run_trials(vec![
        allocation_harness::trial(
            "spectator_admission_reuses_its_owned_room_and_roster",
            spectator_admission_reuses_its_owned_room_and_roster,
        ),
        allocation_harness::trial(
            "runtime_task_registry_initializes_each_shard_once",
            runtime_task_registry_initializes_each_shard_once,
        ),
    ]);
}
