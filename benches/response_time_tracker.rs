use criterion::{criterion_group, criterion_main, Criterion};
use signal_fish_server::metrics::ResponseTimeTracker;
use std::hint::black_box;
use std::time::Duration;

fn bench_response_time_tracker(c: &mut Criterion) {
    c.bench_function("response_time_tracker_record", |b| {
        b.iter(|| {
            let mut tracker = ResponseTimeTracker::new();
            for sample in 0..512u64 {
                let duration = Duration::from_micros(500 + (sample % 250));
                tracker.add_sample("database_query", duration);
            }
            tracker
        });
    });

    c.bench_function("response_time_tracker_get_stats", |b| {
        let mut tracker = ResponseTimeTracker::new();
        for sample in 0..5000u64 {
            let duration = Duration::from_micros(300 + (sample % 200));
            tracker.add_sample("room_join", duration);
        }

        b.iter(|| {
            black_box(tracker.get_latency_metrics("room_join"));
        });
    });
}

criterion_group!(response_time_tracker, bench_response_time_tracker);
criterion_main!(response_time_tracker);
