# Skill: Testing Strategies

<!-- trigger: test, testing, unit, integration, mock, table-driven, async test, naming convention | Core testing methodology and patterns | Core -->

**Trigger**: When writing tests, choosing test patterns, or structuring test code.

---

## When to Use

- Writing unit, integration, or end-to-end tests
- Choosing between test approaches (table-driven, data-driven)
- Setting up async test infrastructure
- Implementing mocks or test doubles
- Testing error paths and edge cases
- Debugging flaky or failing tests

---

## When NOT to Use

- Configuring testing tools or frameworks (see [testing-tools-and-frameworks](./testing-tools-and-frameworks.md))
- Production error handling (see [error-handling-guide](./error-handling-guide.md))
- Benchmark-specific setup (see [rust-performance-optimization](./rust-performance-optimization.md))

---

## TL;DR

- Every change requires tests: happy path, error paths, edge cases, and concurrency.
- Use `#[tokio::test]` for async tests, table-driven tests for validation functions.
- Test names follow `test_<unit>_<condition>_<expected_behavior>` convention.
- Run tests with `cargo nextest` for parallel execution and better output.

> **Note:** In test code, `.unwrap()` and `.expect()` are acceptable — test panics indicate test failures. The strict anti-unwrap policy applies only to production code in `src/`.

---

## Unit Testing Patterns

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_code_validates_length() {
        assert!(RoomCode::new("ABC123").is_ok());
        assert!(RoomCode::new("AB").is_err());
        assert!(RoomCode::new("").is_err());
        assert!(RoomCode::new("ABCDEFGHIJ").is_err());
    }

    #[test]
    fn room_code_rejects_invalid_chars() {
        assert!(RoomCode::new("ABC 23").is_err());  // Space
        assert!(RoomCode::new("ABC!23").is_err());   // Punctuation
        assert!(RoomCode::new("ABC\n23").is_err());  // Newline
    }

    #[test]
    fn room_code_normalizes_to_uppercase() {
        let code = RoomCode::new("abc123").unwrap();
        assert_eq!(code.as_str(), "ABC123");
    }
}
```

### Test Naming Convention

```
test_<unit>_<condition>_<expected_behavior>
```

```rust
#[test] fn room_code_empty_input_returns_invalid_length() { ... }
#[test] fn player_join_room_full_returns_room_full_error() { ... }
#[test] fn broadcast_no_recipients_succeeds_silently() { ... }
```

---

## Data-Driven / Table-Driven Tests

```rust
#[test]
fn test_room_code_validation() {
    let cases = [
        ("ABC123", true, "valid alphanumeric"),
        ("abc123", true, "lowercase normalized"),
        ("AB CD", false, "contains space"),
        ("ABC12", false, "too short"),
        ("", false, "empty"),
        ("ÄÖÜ123", false, "non-ascii"),
    ];

    for (input, expected_valid, desc) in cases {
        let result = RoomCode::new(input);
        assert_eq!(
            result.is_ok(), expected_valid,
            "Case '{desc}': input={input:?}, result={result:?}"
        );
    }
}
```

Include a `desc` string in every case for clear failure messages.

---

## Testing Async Code

```rust
#[tokio::test]
async fn test_websocket_message_handling() {
    let server = TestServer::start().await;
    let mut client = server.connect_ws().await;
    client.send(Message::text(r#"{"type":"ping"}"#)).await.unwrap();

    let response = tokio::time::timeout(Duration::from_secs(5), client.next())
        .await.expect("Timed out").expect("Stream ended").expect("WS error");
    assert_eq!(response, Message::text(r#"{"type":"pong"}"#));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_concurrent_joins() {
    let server = TestServer::start().await;
    let room = server.create_room(4).await;

    let handles: Vec<_> = (0..4).map(|_| {
        let code = room.code.clone();
        let srv = server.clone();
        tokio::spawn(async move { srv.join_room(&code).await })
    }).collect();

    let results: Vec<_> = futures::future::join_all(handles).await
        .into_iter().map(|r| r.unwrap()).collect();
    assert!(results.iter().all(|r| r.is_ok()));
}
```

---

## Mocking and Test Doubles

```rust
struct MockDatabase {
    rooms: HashMap<String, Room>,
    should_fail: bool,
}

#[async_trait]
impl Database for MockDatabase {
    async fn find_room(&self, code: &str) -> Result<Option<Room>, DbError> {
        if self.should_fail { return Err(DbError::ConnectionFailed); }
        Ok(self.rooms.get(code).cloned())
    }
    async fn save_room(&self, _room: &Room) -> Result<(), DbError> {
        if self.should_fail { return Err(DbError::ConnectionFailed); }
        Ok(())
    }
}

#[tokio::test]
async fn test_join_room_db_failure() {
    let db = MockDatabase { rooms: HashMap::new(), should_fail: true };
    let server = GameServer::new(Box::new(db));
    assert!(matches!(server.join_room("ABC123").await, Err(JoinError::Internal(_))));
}
```

---

## Testing Error Paths and Edge Cases

```rust
#[tokio::test]
async fn test_join_after_room_deleted() {
    let server = TestServer::start().await;
    let room = server.create_room(4).await;
    server.delete_room(&room.code).await.unwrap();
    assert!(matches!(server.join_room(&room.code).await, Err(JoinError::RoomNotFound { .. })));
}

#[tokio::test]
async fn test_double_join_same_player() {
    let server = TestServer::start().await;
    let room = server.create_room(4).await;
    let player = server.create_player().await;
    server.join_room_as(&room.code, &player).await.unwrap();
    assert!(matches!(server.join_room_as(&room.code, &player).await, Err(JoinError::AlreadyJoined)));
}
```

Always test: empty collections, Unicode input, zero-value parameters, concurrent access.

---

## Test Organization

Unit tests: `#[cfg(test)] mod tests { }` co-located with code in `src/`. Integration tests: `tests/` directory. Share utilities via `tests/common/mod.rs`.

---

## Testing Concurrent Code

```rust
#[tokio::test(flavor = "multi_thread")]
async fn test_no_data_race_on_room_state() {
    let server = Arc::new(TestServer::start().await);
    let room = server.create_room(100).await;
    let mut set = tokio::task::JoinSet::new();

    for _ in 0..50 {
        let (srv, code) = (Arc::clone(&server), room.code.clone());
        set.spawn(async move { srv.join_room(&code).await });
    }

    while let Some(result) = set.join_next().await {
        result.expect("Task panicked");
    }
}
```

---

## Regression Testing Discipline

```
1. Bug reported → write a failing test FIRST
2. Fix the bug → test passes
3. Test stays forever → prevents regression
```

```rust
// Regression test: Issue #142 — player count not updated on disconnect
#[tokio::test]
async fn regression_142_player_count_after_disconnect() {
    let server = TestServer::start().await;
    let room = server.create_room(4).await;
    let player = server.join_room(&room.code).await.unwrap();

    server.disconnect_player(&player.id).await.unwrap();

    let info = server.room_info(&room.code).await.unwrap();
    assert_eq!(info.player_count, 0, "Player count must be 0 after disconnect");
}
```

---

### Serial Test Isolation

Use `serial_test` (in dev-dependencies) for tests that share global state:

```rust
use serial_test::serial;

#[tokio::test]
#[serial]
async fn test_database_migration() {
    // This test modifies shared database state
    // #[serial] ensures no parallel execution
}
```

---

## Agent Checklist

- [ ] Every change has tests: happy path, error path, edge cases
- [ ] `#[tokio::test]` for async tests
- [ ] Table-driven tests for validation/transformation functions
- [ ] Concurrent tests use `multi_thread` flavor
- [ ] Regression tests cite the issue number
- [ ] Test names follow `test_<unit>_<condition>_<expected>` convention
- [ ] Tests never depend on execution order
- [ ] Flaky tests are treated as bugs — not retried into silence

---

## CI/CD Test Coverage

### Config Validation Tests

Always test that configuration defaults work in production deployment scenarios:

```rust
#[test]
fn test_docker_default_config_passes_validation() {
    // Simulate Docker ENV overrides (auth disabled, no config file)
    let mut config = Config::default();
    config.security.require_metrics_auth = false;
    config.security.require_websocket_auth = false;
    assert!(validate_config_security(&config).is_ok());
}

#[test]
fn test_config_with_all_features_loads() {
    // Ensure config loads with all cargo features enabled
    let config = Config::from_env().unwrap();
    assert!(config.validate().is_ok());
}
```

### Smoke Test Patterns

CI smoke tests must verify the complete deployment artifact:

```yaml
# GitHub Actions example
- name: Smoke test
  run: |
    docker run -d --name test-server -p 3536:3536 signal-fish-server:ci
    # Retry loop instead of bare sleep
    for i in $(seq 1 15); do
      if curl -sf http://localhost:3536/v2/health; then
        echo "Health check passed on attempt $i/15"
        exit 0
      fi
      echo "Attempt $i/15: server not ready, retrying in 2s..."
      sleep 2
    done
    echo "ERROR: Server failed to become healthy after 30s"
    echo "=== Docker logs ==="
    docker logs test-server
    exit 1
```

**Key smoke test requirements:**

- Retry loop with timeout (not bare `sleep`)
- Dump logs on failure for diagnostics
- Test default configuration (no mounted config files)
- Verify all critical endpoints (health, metrics, WebSocket upgrade)

### File Path Case Sensitivity Tests

```rust
#[test]
fn test_skill_links_case_sensitive() {
    // Verify all skill file links use correct case (prevents Linux CI failures)
    let context_file = std::fs::read_to_string(".llm/context.md").unwrap();
    for (skill_name, skill_path) in extract_skill_links(&context_file) {
        assert!(
            std::path::Path::new(skill_path).exists(),
            "Skill link broken: {skill_name} -> {skill_path}"
        );
    }
}
```

### CI-Specific Integration Tests

```rust
#[cfg(test)]
mod ci_integration_tests {
    use super::*;

    #[test]
    #[ignore = "runs only in CI"]
    fn test_all_features_compile() {
        // This test verifies --all-features builds succeed
        // Ignored by default, runs only in CI via `cargo test -- --ignored`
    }

    #[test]
    fn test_native_deps_available() {
        // Verify native dependencies required by optional features are present
        #[cfg(feature = "kafka")]
        {
            // Test that rdkafka native lib is available
            let _ = rdkafka::ClientConfig::new();
        }
    }
}
```

---

## Related Skills

- [testing-tools-and-frameworks](./testing-tools-and-frameworks.md) — Testing tools, frameworks, and coverage measurement
- [rust-refactoring-guide](./rust-refactoring-guide.md) — Tests must pass before and after refactoring
- [error-handling-guide](./error-handling-guide.md) — Testing error conditions
- [defensive-programming](./defensive-programming.md) — Edge cases to test
- [clippy-and-linting](./clippy-and-linting.md) — CI pipeline integration
- [github-actions-best-practices](./github-actions-best-practices.md) — GitHub Actions workflow patterns and debugging
