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

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = relay_serialization_runtime, payload_sizing_runtime
}
criterion_main!(benches);
