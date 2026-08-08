//! Allocation benchmark for production relay fan-out plus socket projection.
//!
//! Run explicitly:
//!
//! ```text
//! cargo bench --bench relay_serialization_allocations --features allocation-tracking
//! ```

#[path = "support/relay_serialization.rs"]
mod relay_serialization;

use relay_serialization::{
    assert_expected_output_digest, Fixture, Ledger, Scenario, RELAYS_PER_SAMPLE, ROOM_SIZES,
};
use stats_alloc::{Region, Stats, StatsAlloc, INSTRUMENTED_SYSTEM};
use std::alloc::System;

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const REPEATS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sample {
    stats: Stats,
    ledger: Ledger,
}

fn repeated_sample(fixture: &mut Fixture) -> Sample {
    let region = Region::new(GLOBAL);
    let ledger = fixture.run_sample();
    let first = Sample {
        stats: region.change(),
        ledger,
    };

    for repeat in 1..REPEATS {
        let region = Region::new(GLOBAL);
        let ledger = fixture.run_sample();
        let observed = Sample {
            stats: region.change(),
            ledger,
        };
        assert_eq!(
            observed, first,
            "serialization allocation sample {repeat} drifted; setup or background work \
             contaminated the measurement"
        );
    }
    first
}

fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn print_sample(scenario: Scenario, room_size: usize, sample: &Sample) {
    let recipients = room_size - 1;
    let allocation_operations = sample.stats.allocations + sample.stats.reallocations;
    println!(
        "{},{room_size},{recipients},{RELAYS_PER_SAMPLE},{},{},{},{},{},{},{},{},{},{},{},{},{:.4},{:.2},{:.4},{:.2}",
        scenario.name(),
        sample.ledger.materialized,
        sample.ledger.text_frames,
        sample.ledger.binary_frames,
        sample.ledger.wire_bytes,
        sample.ledger.json_encodes,
        sample.ledger.message_pack_encodes,
        sample.ledger.message_pack_decodes,
        hex_digest(&sample.ledger.output_sha256),
        sample.stats.allocations,
        sample.stats.reallocations,
        sample.stats.deallocations,
        sample.stats.bytes_allocated,
        allocation_operations as f64 / RELAYS_PER_SAMPLE as f64,
        sample.stats.bytes_allocated as f64 / RELAYS_PER_SAMPLE as f64,
        allocation_operations as f64 / sample.ledger.materialized as f64,
        sample.stats.bytes_allocated as f64 / sample.ledger.materialized as f64,
    );
}

fn assert_allocation_ceiling(scenario: Scenario, room_size: usize, sample: &Sample) {
    let (maximum_operations_per_relay, maximum_reallocations_per_relay, maximum_bytes_per_relay) =
        match (scenario, room_size) {
            (Scenario::V2JsonBinary | Scenario::V2RkyvBinary, 2) => (2, 0, 425),
            (Scenario::V2JsonBinary | Scenario::V2RkyvBinary, 8) => (2, 0, 665),
            (Scenario::V2JsonBinary | Scenario::V2RkyvBinary, 16) => (2, 0, 985),
            (Scenario::V3JsonText, 2) => (7, 3, 2_587),
            (Scenario::V3JsonText, 8 | 16) => (8, 3, 3_059),
            (Scenario::V3MessagePackBinary, 2) => (4, 0, 1_581),
            (Scenario::V3MessagePackBinary, 8 | 16) => (5, 0, 2_053),
            (Scenario::MixedMessagePackSource, 2) => (14, 0, 3_572),
            (Scenario::MixedMessagePackSource, 8 | 16) => (21, 0, 7_642),
            _ => panic!("room-{room_size} has no checked-in allocation baseline"),
        };
    let allocation_operations = sample.stats.allocations + sample.stats.reallocations;
    // Every 1,024-relay cell has one deterministic sample-scoped operation
    // (the `.0010` visible in the report), so keep that fixed allowance
    // separate from the integral per-relay hot-path ceiling.
    let maximum_operations = maximum_operations_per_relay * RELAYS_PER_SAMPLE + 1;
    let maximum_reallocations = maximum_reallocations_per_relay * RELAYS_PER_SAMPLE;
    let maximum_bytes = maximum_bytes_per_relay * RELAYS_PER_SAMPLE;
    assert!(
        allocation_operations <= maximum_operations,
        "{} room-{room_size} projection used {allocation_operations} allocation operations across \
         {RELAYS_PER_SAMPLE} relays; expected at most {maximum_operations}",
        scenario.name()
    );
    assert!(
        sample.stats.reallocations <= maximum_reallocations,
        "{} room-{room_size} projection used {} reallocations across {RELAYS_PER_SAMPLE} relays; \
         expected at most {maximum_reallocations}",
        scenario.name(),
        sample.stats.reallocations
    );
    assert!(
        sample.stats.bytes_allocated <= maximum_bytes,
        "{} room-{room_size} projection allocated {} bytes across {RELAYS_PER_SAMPLE} relays; \
         expected at most {maximum_bytes}",
        scenario.name(),
        sample.stats.bytes_allocated
    );
}

fn main() {
    println!(
        "scenario,room_size,recipients,relays,materialized,text_frames,binary_frames,\
         wire_bytes,json_encodes,message_pack_encodes,message_pack_decodes,output_sha256,\
         allocations,reallocations,deallocations,bytes_allocated,allocation_ops_per_relay,\
         bytes_per_relay,allocation_ops_per_delivery,bytes_per_delivery"
    );

    for scenario in Scenario::ALL {
        for room_size in ROOM_SIZES {
            let mut fixture = Fixture::new(room_size, scenario);
            let sample = repeated_sample(&mut fixture);
            assert_expected_output_digest(scenario, room_size, &sample.ledger);
            assert_allocation_ceiling(scenario, room_size, &sample);
            print_sample(scenario, room_size, &sample);
        }
    }
}
