//! Runtime benchmark for production relay fan-out plus socket projection.
//!
//! The allocator is deliberately not instrumented here; `stats_alloc` uses a
//! sequentially consistent atomic operation per allocation and would distort
//! the timing result.

#[path = "support/relay_serialization.rs"]
mod relay_serialization;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use relay_serialization::{
    assert_expected_output_digest, Fixture, Scenario, RELAYS_PER_SAMPLE, ROOM_SIZES,
};
use std::hint::black_box;
use std::time::Duration;

fn relay_serialization_runtime(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("relay_serialization");
    group.sample_size(20);

    for scenario in Scenario::ALL {
        for room_size in ROOM_SIZES {
            let mut fixture = Fixture::new(room_size, scenario);
            // Exact wire hashing is validated before measurement. Timed
            // iterations retain the production projection and cheap ledger,
            // but omit SHA-256 work the server never performs.
            let validated = fixture.run_sample();
            assert_expected_output_digest(scenario, room_size, &validated);
            let expected = fixture.run_timed_sample();
            group.throughput(Throughput::Elements(
                (RELAYS_PER_SAMPLE * (room_size - 1)) as u64,
            ));
            group.bench_with_input(
                BenchmarkId::new(scenario.name(), room_size),
                &room_size,
                |bencher, _| {
                    bencher.iter(|| {
                        let observed = fixture.run_timed_sample();
                        assert_eq!(
                            observed, expected,
                            "timed production projection changed its non-vacuity ledger"
                        );
                        black_box(observed)
                    });
                },
            );
        }
    }
    group.finish();
}

/// Compare production JSON sizing CPU time with the serializer reference.
fn payload_sizing_runtime(criterion: &mut Criterion) {
    use signal_fish_server::server::allocation_benchmark::{
        canonical_json_len, game_data_json_len,
    };

    let mut group = criterion.benchmark_group("payload_sizing");
    group.sample_size(20);
    for (name, payload) in [
        ("ascii", serde_json::json!({"snapshot": "x".repeat(4096)})),
        ("ascii96", serde_json::json!({"snapshot": "x".repeat(96)})),
        (
            "unicode",
            serde_json::json!({"snapshot": "é雪😀".repeat(512)}),
        ),
        (
            "floats",
            serde_json::json!((0..64)
                .map(|value| f64::from(value) / 7.0)
                .collect::<Vec<_>>()),
        ),
        (
            "escaped",
            serde_json::json!({"snapshot": "\n\t\u{0000}\"\\é".repeat(512)}),
        ),
        ("numbers", serde_json::json!((0..512).collect::<Vec<u64>>())),
    ] {
        let expected = serde_json::to_vec(&payload)
            .expect("canonical payload")
            .len();
        assert_eq!(game_data_json_len(&payload), expected);
        group.throughput(Throughput::Bytes(expected as u64));
        group.bench_function(name, |bencher| {
            bencher.iter(|| black_box(game_data_json_len(black_box(&payload))));
        });
        group.bench_function(format!("{name}/serde"), |bencher| {
            bencher.iter(|| black_box(canonical_json_len(black_box(&payload))));
        });
    }
    group.finish();
}

/// Compare exact offer, ICE and opaque payload sizing against serialization.
fn signal_payload_sizing(criterion: &mut Criterion) {
    use signal_fish_server::server::allocation_benchmark::{
        canonical_json_len, game_data_json_len,
    };
    let mut group = criterion.benchmark_group("signal_payload_sizing");
    for (name, payload) in [
        (
            "offer",
            serde_json::json!({"Offer": format!("v=0\r\n{}", "a=candidate:1 1 UDP 2122260223 192.0.2.1 5000 typ host\r\n".repeat(64))}),
        ),
        (
            "ice",
            serde_json::json!({"IceCandidate": {"candidate": "candidate:1 1 UDP 2122260223 192.0.2.1 5000 typ host", "sdpMid": "0", "sdpMLineIndex": 0}}),
        ),
        (
            "opaque",
            serde_json::json!({"nested": ["雪\n\t\u{0000}\"\\".repeat(128), -1, 0.25, null]}),
        ),
    ] {
        let expected = serde_json::to_vec(&payload)
            .expect("signal serialization")
            .len();
        assert_eq!(game_data_json_len(&payload), expected);
        assert_eq!(canonical_json_len(&payload), expected);
        for (implementation, count) in [
            (
                "value",
                game_data_json_len as fn(&serde_json::Value) -> usize,
            ),
            (
                "serde",
                canonical_json_len::<serde_json::Value> as fn(&serde_json::Value) -> usize,
            ),
        ] {
            group.bench_function(format!("{name}/{implementation}"), |bencher| {
                bencher.iter(|| black_box(count(black_box(&payload))));
            });
        }
    }
    group.finish();
}

/// Measure complete star publications with the host last in the roster.
fn host_plan_projection(criterion: &mut Criterion) {
    use signal_fish_server::server::allocation_benchmark::HostElectionFixture;
    let mut group = criterion.benchmark_group("host_plan_projection");
    for direct in [false, true] {
        for count in [2, 16, 64, 255] {
            let mut fixture = HostElectionFixture::new(count, direct);
            let plans = fixture.plans_for_members();
            assert_eq!(plans.len(), count as usize);
            assert_eq!(
                plans.iter().map(|plan| plan.peers.len()).sum::<usize>(),
                2 * (count as usize - 1)
            );
            group.bench_function(format!("{direct}/{count}"), |bencher| {
                bencher.iter(|| black_box(fixture.plans_for_members()));
            });
        }
    }
    group.finish();
}

/// Measure complete mesh publications with every member paired.
fn mesh_plan_projection(criterion: &mut Criterion) {
    use signal_fish_server::server::allocation_benchmark::HostElectionFixture;
    let mut group = criterion.benchmark_group("mesh_plan_projection");
    for count in [2, 16, 64, 255] {
        let mut fixture = HostElectionFixture::new(count, false);
        let plans = fixture.mesh_plans_for_members();
        assert_eq!(plans.len(), count as usize);
        assert_eq!(
            plans.iter().map(|plan| plan.peers.len()).sum::<usize>(),
            count as usize * (count as usize - 1)
        );
        group.bench_function(count.to_string(), |bencher| {
            bencher.iter(|| black_box(fixture.mesh_plans_for_members()));
        });
    }
    group.finish();
}

/// Compare room readiness projections with exact nested-scan and sort references.
fn readiness_projection(criterion: &mut Criterion) {
    use signal_fish_server::protocol::{PlayerId, PlayerInfo};
    use signal_fish_server::server::allocation_benchmark::apply_snapshot_readiness;
    let mut group = criterion.benchmark_group("readiness_projection");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(200));
    group.measurement_time(Duration::from_millis(700));
    let identity = |index: u128| {
        PlayerId::from_u128(
            index
                .wrapping_mul(0x9e3779b97f4a7c15_d1b54a32d192ed03)
                .rotate_left(37),
        )
    };
    for (count, ready_count) in [
        (2_u128, 2_u128),
        (16, 16),
        (64, 64),
        (128, 128),
        (255, 255),
        (2, 255),
        (255, 2),
        (64, 255),
        (255, 64),
        (127, 128),
        (128, 127),
        (128, 129),
        (129, 128),
    ] {
        for (shape, step, offset) in [("full", 1, 0), ("sparse", 2, 0), ("disjoint", 1, 1000)] {
            let mut players: Vec<_> = (0..count)
                .map(|index| PlayerInfo {
                    id: identity(index + 1),
                    name: String::new(),
                    is_authority: false,
                    is_ready: false,
                    connected_at: None,
                    connection_info: None,
                    epoch: None,
                    seq: None,
                    region_id: String::new(),
                })
                .collect();
            let ready: Vec<_> = (0..ready_count)
                .rev()
                .map(|index| identity(index * step + offset + 1))
                .collect();
            apply_snapshot_readiness(&mut players, &ready);
            assert!(players
                .iter()
                .all(|player| player.is_ready == ready.contains(&player.id)));
            let case = format!("{shape}/{count}/{ready_count}");
            group.bench_function(format!("production/{case}"), |bencher| {
                bencher
                    .iter(|| apply_snapshot_readiness(black_box(&mut players), black_box(&ready)));
            });
            group.bench_function(format!("sorted_copy/{case}"), |bencher| {
                bencher.iter(|| {
                    let mut sorted = black_box(&ready).clone();
                    sorted.sort_unstable();
                    for player in black_box(&mut players) {
                        player.is_ready = sorted.binary_search(&player.id).is_ok();
                    }
                });
            });
            group.bench_function(format!("nested_scan/{case}"), |bencher| {
                bencher.iter(|| {
                    for player in black_box(&mut players) {
                        player.is_ready = black_box(&ready).contains(&player.id);
                    }
                });
            });
        }
    }
    group.finish();
}

/// Compare complete membership construction and lookup with the hash-table baseline.
fn effective_membership(criterion: &mut Criterion) {
    use signal_fish_server::protocol::PlayerId;
    use signal_fish_server::server::allocation_benchmark::effective_room_members;
    use std::collections::HashSet;
    let mut group = criterion.benchmark_group("effective_membership");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(100));
    group.measurement_time(Duration::from_millis(400));
    for count in [2_u128, 16, 255, 1024] {
        let identity = |index: u128| {
            PlayerId::from_u128(
                index
                    .wrapping_mul(0x9e3779b97f4a7c15_d1b54a32d192ed03)
                    .rotate_left(37),
            )
        };
        let stored: Vec<_> = (0..count).map(identity).collect();
        for (shape, routed) in [
            (
                "full",
                Some(stored.iter().rev().copied().collect::<Vec<_>>()),
            ),
            (
                "partial",
                Some((0..count * 2).step_by(3).map(identity).collect()),
            ),
            ("unknown", None),
        ] {
            let old =
                |mut stored: Vec<PlayerId>, routed: Option<Vec<PlayerId>>| -> HashSet<PlayerId> {
                    match routed {
                        Some(routed) => {
                            let mut current: HashSet<_> = routed.into_iter().collect();
                            stored.retain(|id| current.contains(id));
                            current.clear();
                            current.extend(stored);
                            current
                        }
                        None => stored.into_iter().collect(),
                    }
                };
            let expected = old(stored.clone(), routed.clone());
            let effective = effective_room_members(stored.clone(), routed.clone());
            assert_eq!(effective.iter().copied().collect::<HashSet<_>>(), expected);
            for original in [false, true] {
                group.bench_function(format!("{original}/{shape}/{count}"), |bencher| {
                    bencher.iter_batched(
                        || (stored.clone(), routed.clone()),
                        |(stored, routed)| {
                            let found = if original {
                                let effective = old(stored, routed);
                                expected.iter().filter(|id| effective.contains(id)).count()
                            } else {
                                let effective = effective_room_members(stored, routed);
                                expected
                                    .iter()
                                    .filter(|id| effective.binary_search(id).is_ok())
                                    .count()
                            };
                            assert_eq!(black_box(found), expected.len());
                        },
                        criterion::BatchSize::SmallInput,
                    );
                });
            }
        }
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = effective_membership, readiness_projection, relay_serialization_runtime, payload_sizing_runtime, host_plan_projection, mesh_plan_projection, signal_payload_sizing
}
criterion_main!(benches);
