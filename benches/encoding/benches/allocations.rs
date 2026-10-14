#[allow(warnings, clippy::all)]
#[path = "../generated/game_capnp.rs"]
pub mod game_capnp;
#[path = "../support/mod.rs"]
mod support;
use stats_alloc::{Region, Stats, StatsAlloc, INSTRUMENTED_SYSTEM};
use std::alloc::System;
use std::hint::black_box;
use support::{fixtures, validate, Codec};
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn sample<T>(operation: impl Fn() -> T) -> Stats {
    let mut baseline = None;
    for _ in 0..5 {
        let region = Region::new(GLOBAL);
        let output = black_box(operation());
        let stats = region.change();
        drop(output);
        if let Some(expected) = baseline {
            assert_eq!(stats, expected, "allocation sample drift");
        }
        baseline = Some(stats);
    }
    baseline.unwrap()
}
fn main() {
    println!("fixture,corpus_sha256,json_bytes,codec,encoded_bytes,operation,allocations,reallocations,allocation_ops,bytes_allocated");
    for fixture in fixtures() {
        for codec in Codec::ALL {
            let bytes = validate(codec, &fixture);
            let encode = sample(|| codec.encode(black_box(&fixture.value)));
            let decode = sample(|| codec.decode(fixture.value.shape(), black_box(&bytes)));
            for (operation, stats) in [("encode", encode), ("decode", decode)] {
                println!(
                    "{},{},{},{},{},{},{},{},{},{}",
                    fixture.name,
                    fixture.sha256,
                    fixture.source_bytes,
                    codec.name(),
                    bytes.len(),
                    operation,
                    stats.allocations,
                    stats.reallocations,
                    stats.allocations + stats.reallocations,
                    stats.bytes_allocated
                );
            }
        }
    }
}
