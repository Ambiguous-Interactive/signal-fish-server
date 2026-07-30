//! Runtime benchmark for production relay fan-out plus socket projection.
//!
//! The allocator is deliberately not instrumented here; `stats_alloc` uses a
//! sequentially consistent atomic operation per allocation and would distort
//! the timing result.

#[path = "support/relay_serialization.rs"]
mod relay_serialization;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use relay_serialization::{Fixture, Scenario, RELAYS_PER_SAMPLE, ROOM_SIZES};
use std::hint::black_box;
use std::time::Duration;

fn relay_serialization_runtime(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("relay_serialization");
    group.sample_size(20);

    for scenario in Scenario::ALL {
        for room_size in ROOM_SIZES {
            let mut fixture = Fixture::new(room_size, scenario);
            let expected = fixture.run_sample();
            group.throughput(Throughput::Elements(
                (RELAYS_PER_SAMPLE * (room_size - 1)) as u64,
            ));
            group.bench_with_input(
                BenchmarkId::new(scenario.name(), room_size),
                &room_size,
                |bencher, _| {
                    bencher.iter(|| {
                        let observed = fixture.run_sample();
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

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3));
    targets = relay_serialization_runtime
}
criterion_main!(benches);
