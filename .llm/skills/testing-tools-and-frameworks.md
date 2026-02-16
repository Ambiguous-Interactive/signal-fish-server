# Skill: Testing Tools and Frameworks

<!-- trigger: testcontainers, proptest, insta, criterion, fuzz, nextest, coverage, tarpaulin | Testing tools, frameworks, and coverage measurement | Core -->

**Trigger**: When choosing or configuring testing tools, frameworks, or coverage measurement.

---

## When to Use

- Setting up testcontainers for database integration tests
- Writing property-based tests with proptest
- Using snapshot testing with insta
- Running benchmarks with criterion
- Configuring cargo-nextest or code coverage tools
- Fuzz testing with cargo-fuzz

---

## When NOT to Use

- Core test methodology and patterns (see [testing-strategies](./testing-strategies.md))
- Production error handling (see [error-handling-guide](./error-handling-guide.md))

---

## TL;DR

- Use `testcontainers` for database integration tests with real PostgreSQL.
- Use `proptest` for property-based testing of serialization and invariants.
- Use `insta` for snapshot testing of serialized outputs.
- Use `criterion` for benchmarks, `cargo-tarpaulin` or `llvm-cov` for coverage.
- Use `cargo nextest` as the default test runner for parallel execution.

---

## Integration Testing with testcontainers

```rust
use testcontainers::{runners::AsyncRunner, GenericImage};

#[tokio::test]
async fn test_room_persistence_in_postgres() {
    // Start a real PostgreSQL container
    let container = GenericImage::new("postgres", "16")
        .with_env_var("POSTGRES_PASSWORD", "test")
        .with_env_var("POSTGRES_DB", "matchbox_test")
        .start()
        .await
        .expect("Failed to start PostgreSQL container");

    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let db_url = format!("postgres://postgres:test@localhost:{port}/matchbox_test");

    // Run migrations and test
    let pool = PgPool::connect(&db_url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let db = PostgresDatabase::new(pool);
    let room = db.create_room(test_room_config()).await.unwrap();
    let found = db.find_room(&room.code).await.unwrap();

    assert_eq!(found.unwrap().code, room.code);
}
```rust

---

## Property-Based Testing with proptest

```rust
use proptest::prelude::*;

proptest! {
    // Test that serialization round-trips for any valid message
    #[test]
    fn message_roundtrip(msg in arb_message()) {
        let bytes = serde_json::to_vec(&msg).unwrap();
        let decoded: Message = serde_json::from_slice(&bytes).unwrap();
        prop_assert_eq!(msg, decoded);
    }

    // Test that room code validation is consistent
    #[test]
    fn room_code_validation_never_panics(s in "\\PC{0,100}") {
        // Should never panic — always returns Ok or Err
        let _ = RoomCode::new(&s);
    }

    // Test that player count never exceeds max
    #[test]
    fn room_never_exceeds_max_players(
        max in 1u32..=100,
        attempts in 1u32..=200,
    ) {
        let mut room = Room::new(RoomConfig { max_players: max, ..Default::default() });
        for i in 0..attempts {
            let _ = room.add_player(PlayerId::new());
        }
        prop_assert!(room.player_count() <= max as usize);
    }
}

// Custom strategy for generating valid messages
fn arb_message() -> impl Strategy<Value = Message> {
    prop_oneof![
        Just(Message::Ping),
        Just(Message::Pong),
        any::<Vec<u8>>().prop_map(|data| Message::Data(data.into())),
        "[A-Z0-9]{6}".prop_map(|code| Message::JoinRoom(code)),
    ]
}
```

---

## Snapshot Testing with insta

> **Note:** `insta` is not currently in dev-dependencies. Add `insta = "1.x"` to `[dev-dependencies]` to use snapshot testing.

```rust
use insta::assert_json_snapshot;

#[test]
fn test_room_info_serialization() {
    let info = RoomInfo {
        code: RoomCode::new("ABC123").unwrap(),
        player_count: 3,
        max_players: 8,
        created_at: DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z").unwrap(),
    };

    // Creates/updates snapshot file in snapshots/ directory
    assert_json_snapshot!(info, @r###"
    {
      "code": "ABC123",
      "player_count": 3,
      "max_players": 8,
      "created_at": "2025-01-01T00:00:00Z"
    }
    "###);
}

// Review changes: cargo insta review
```rust

---

## Benchmark Testing with Criterion

```rust
// benches/room_operations.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_room_create(c: &mut Criterion) {
    c.bench_function("create_room", |b| {
        b.iter(|| {
            Room::new(black_box(RoomConfig::default()))
        })
    });
}

fn bench_broadcast(c: &mut Criterion) {
    let room = setup_room_with_players(100);
    let msg = Bytes::from_static(b"test message");

    c.bench_function("broadcast_100_players", |b| {
        b.iter(|| {
            room.broadcast(black_box(msg.clone()))
        })
    });
}

fn bench_room_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("room_lookup");
    for size in [10, 100, 1000, 10000] {
        let server = setup_server_with_rooms(size);
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &size,
            |b, _| {
                b.iter(|| server.find_room(black_box(&test_code())))
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_room_create, bench_broadcast, bench_room_lookup);
criterion_main!(benches);
```

---

## Fuzz Testing Basics

```rust
// fuzz/fuzz_targets/parse_message.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Should never panic, regardless of input
    let _ = parse_client_message(data);
});

// Run: cargo +nightly fuzz run parse_message
```rust

---

## HTTP Handler Testing with axum-test

Use `axum-test` (in dev-dependencies) for testing axum handlers:

```rust
use axum_test::TestServer;

#[tokio::test]
async fn test_health_endpoint() {
    let app = create_app();
    let server = TestServer::new(app).unwrap();
    let response = server.get("/health").await;
    response.assert_status_ok();
}
```

---

## cargo-nextest for Faster Execution

```bash
# Install
cargo install cargo-nextest

# Run all tests (parallel by default)
cargo nextest run --all-features

# Run with retries for flaky-test investigation
cargo nextest run --retries 2

# Run specific test
cargo nextest run test_room_creation

# Filter by test name pattern
cargo nextest run -E 'test(websocket)'

# With output capture on failure only
cargo nextest run --failure-output immediate
```bash

---

## Code Coverage

```bash
# Using cargo-tarpaulin (configured in tarpaulin.toml)
cargo tarpaulin --all-features --out html

# Using llvm-cov for more accurate coverage
cargo llvm-cov --all-features --html
cargo llvm-cov --all-features --lcov --output-path lcov.info
```

---

## Agent Checklist

- [ ] `proptest` for serialization round-trip and invariant tests
- [ ] `testcontainers` for database integration tests
- [ ] `insta` for snapshot testing serialized outputs
- [ ] `criterion` benchmarks for performance-critical paths
- [ ] `cargo nextest` as default test runner
- [ ] `cargo tarpaulin` or `llvm-cov` for coverage reports
- [ ] `axum-test` for HTTP handler testing
- [ ] Fuzz targets for parser and protocol code

---

## Related Skills

- [testing-strategies](./testing-strategies.md) — Core testing methodology and patterns
- [clippy-and-linting](./clippy-and-linting.md) — CI pipeline integration
- [rust-performance-optimization](./rust-performance-optimization.md) — Benchmark setup with criterion
- [async-rust-best-practices](./async-rust-best-practices.md) — Async test patterns
