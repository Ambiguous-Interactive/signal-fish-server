#[allow(warnings, clippy::all)]
#[path = "../generated/game_capnp.rs"]
pub mod game_capnp;
#[path = "../support/mod.rs"]
mod support;
use criterion::{BenchmarkId, Criterion};
use std::hint::black_box;
use std::time::Duration;
use support::{fixtures, validate, Codec};

fn main() {
    let mut criterion = Criterion::default()
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2))
        .configure_from_args();
    for fixture in fixtures() {
        let mut group = criterion.benchmark_group(&fixture.name);
        for codec in Codec::ALL {
            let bytes = validate(codec, &fixture);
            group.bench_with_input(
                BenchmarkId::new(codec.name(), "encode"),
                &fixture.value,
                |bench, value| bench.iter(|| codec.encode(black_box(value))),
            );
            group.bench_with_input(
                BenchmarkId::new(codec.name(), "decode"),
                &bytes,
                |bench, bytes| bench.iter(|| codec.decode(fixture.value.shape(), black_box(bytes))),
            );
        }
        group.finish();
    }
    criterion.final_summary();
}
