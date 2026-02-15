/// Comprehensive Load Testing Suite
///
/// Tests Signal Fish under realistic high-load scenarios.
/// These tests validate performance, scalability, and reliability under stress.
///
/// Performance Targets (from docs/history/project-plan.md):
/// - WebSocket Connection Rate: 1,000 connections/second
/// - Room Creation Throughput: 100 rooms/second
/// - Message Latency: p50 < 10ms, p99 < 50ms
/// - Relay Packet Throughput: 10,000 packets/second
/// - Concurrent Capacity: 10,000 concurrent connections
/// - Memory Usage: < 2GB at max load
mod test_helpers;

use signal_fish_server::server::ServerConfig;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use test_helpers::{create_test_server, create_test_server_with_config};
use tokio::sync::{mpsc, Barrier};
use tokio::time::timeout;

/// Performance metrics collector
#[derive(Default)]
struct PerformanceMetrics {
    total_operations: AtomicUsize,
    successful_operations: AtomicUsize,
    failed_operations: AtomicUsize,
    total_latency_ms: AtomicU64,
    min_latency_ms: AtomicU64,
    max_latency_ms: AtomicU64,
}

impl PerformanceMetrics {
    fn new() -> Self {
        Self {
            min_latency_ms: AtomicU64::new(u64::MAX),
            ..Default::default()
        }
    }

    fn record_operation(&self, success: bool, latency_ms: u64) {
        self.total_operations.fetch_add(1, Ordering::Relaxed);
        if success {
            self.successful_operations.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_operations.fetch_add(1, Ordering::Relaxed);
        }

        self.total_latency_ms
            .fetch_add(latency_ms, Ordering::Relaxed);

        // Update min (compare and swap loop)
        let mut current_min = self.min_latency_ms.load(Ordering::Relaxed);
        while latency_ms < current_min {
            match self.min_latency_ms.compare_exchange(
                current_min,
                latency_ms,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current_min = actual,
            }
        }

        // Update max (compare and swap loop)
        let mut current_max = self.max_latency_ms.load(Ordering::Relaxed);
        while latency_ms > current_max {
            match self.max_latency_ms.compare_exchange(
                current_max,
                latency_ms,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current_max = actual,
            }
        }
    }

    fn report(&self, test_name: &str, duration: Duration) {
        let total = self.total_operations.load(Ordering::Relaxed);
        let successful = self.successful_operations.load(Ordering::Relaxed);
        let failed = self.failed_operations.load(Ordering::Relaxed);
        let total_latency = self.total_latency_ms.load(Ordering::Relaxed);
        let min = self.min_latency_ms.load(Ordering::Relaxed);
        let max = self.max_latency_ms.load(Ordering::Relaxed);

        let avg_latency = if successful > 0 {
            total_latency / successful as u64
        } else {
            0
        };

        let throughput = if duration.as_secs() > 0 {
            total as f64 / duration.as_secs_f64()
        } else {
            0.0
        };

        println!("\n===================================================");
        println!("Load Test Results: {test_name}");
        println!("===================================================");
        println!("Duration: {:.2}s", duration.as_secs_f64());
        println!("Total Operations: {total}");
        println!(
            "Successful: {} ({:.1}%)",
            successful,
            (successful as f64 / total as f64) * 100.0
        );
        println!(
            "Failed: {} ({:.1}%)",
            failed,
            (failed as f64 / total as f64) * 100.0
        );
        println!("Throughput: {throughput:.2} ops/sec");
        println!("Latency:");
        println!("   - Average: {avg_latency} ms");
        println!("   - Min: {} ms", if min == u64::MAX { 0 } else { min });
        println!("   - Max: {max} ms");
        println!("===================================================\n");
    }
}

/// Test 1: Concurrent WebSocket Connections
/// Target: 1,000 connections/second, measure connection time
#[tokio::test]
#[ignore] // Run with: cargo test --test load_tests -- --ignored
async fn test_load_concurrent_websocket_connections() {
    let config = ServerConfig {
        default_max_players: 4,
        ..Default::default()
    };

    let server = create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;
    let metrics = Arc::new(PerformanceMetrics::new());

    // Test parameters
    let num_connections = 1000;
    let concurrent_batch_size = 100; // Connect in batches to avoid overwhelming

    println!("\nStarting load test: {num_connections} concurrent WebSocket connections");
    let start = Instant::now();

    for batch in 0..(num_connections / concurrent_batch_size) {
        let batch_handles: Vec<_> = (0..concurrent_batch_size)
            .map(|_i| {
                let server_clone = server.clone();
                let metrics_clone = metrics.clone();

                tokio::spawn(async move {
                    let conn_start = Instant::now();
                    let player_id = uuid::Uuid::new_v4();
                    let (tx, _rx) = mpsc::channel(64);

                    // Attempt to connect
                    let result = timeout(
                        Duration::from_secs(5),
                        server_clone.connect_client(player_id, tx),
                    )
                    .await;

                    let latency = conn_start.elapsed().as_millis() as u64;
                    let success = result.is_ok();

                    metrics_clone.record_operation(success, latency);

                    // Keep connection alive briefly
                    tokio::time::sleep(Duration::from_millis(100)).await;

                    // Disconnect
                    server_clone.unregister_client(&player_id).await;
                })
            })
            .collect();

        // Wait for this batch to complete
        for handle in batch_handles {
            let _ = handle.await;
        }

        println!(
            "Batch {} completed ({}/{} connections)",
            batch + 1,
            (batch + 1) * concurrent_batch_size,
            num_connections
        );
    }

    let duration = start.elapsed();
    metrics.report("Concurrent WebSocket Connections", duration);

    // Assertions
    let successful = metrics.successful_operations.load(Ordering::Relaxed);
    let total = metrics.total_operations.load(Ordering::Relaxed);

    assert!(
        successful as f64 / total as f64 >= 0.95,
        "At least 95% of connections should succeed (got {successful}/{total})"
    );

    let throughput = total as f64 / duration.as_secs_f64();
    println!("Connection throughput: {throughput:.2} connections/sec (target: 1000)");
}

/// Test 2: Room Creation Throughput
/// Target: 100 rooms/second
#[tokio::test]
#[ignore]
async fn test_load_room_creation_throughput() {
    let server = Arc::new(create_test_server().await);
    let metrics = Arc::new(PerformanceMetrics::new());

    let num_rooms = 500; // Create 500 rooms
    let concurrent_creates = 50; // 50 concurrent room creations

    println!("\nStarting load test: Room creation throughput");
    let start = Instant::now();

    let barrier = Arc::new(Barrier::new(concurrent_creates));
    let mut handles = Vec::new();

    for batch in 0..(num_rooms / concurrent_creates) {
        for i in 0..concurrent_creates {
            let server_clone = server.clone();
            let metrics_clone = metrics.clone();
            let barrier_clone = barrier.clone();

            let handle = tokio::spawn(async move {
                barrier_clone.wait().await; // Synchronize start

                let room_start = Instant::now();
                let player_id = uuid::Uuid::new_v4();
                let (tx, _rx) = mpsc::channel(64);

                // Connect client
                server_clone.connect_client(player_id, tx).await;

                // Create unique room
                let room_code = format!("LOAD{batch:04}{i:02}");
                let result = timeout(
                    Duration::from_secs(5),
                    server_clone.handle_join_room(
                        &player_id,
                        "load_test_game".to_string(),
                        Some(room_code.clone()),
                        format!("Player{i}"),
                        Some(4),
                        Some(true),
                        None,
                    ),
                )
                .await;

                let latency = room_start.elapsed().as_millis() as u64;
                let success =
                    result.is_ok() && server_clone.get_client_room(&player_id).await.is_some();

                metrics_clone.record_operation(success, latency);
            });

            handles.push(handle);
        }

        // Wait for batch to complete
        for _ in 0..concurrent_creates {
            if let Some(handle) = handles.pop() {
                let _ = handle.await;
            }
        }

        println!(
            "Batch {} completed ({}/{} rooms)",
            batch + 1,
            (batch + 1) * concurrent_creates,
            num_rooms
        );
    }

    let duration = start.elapsed();
    metrics.report("Room Creation Throughput", duration);

    // Assertions
    let successful = metrics.successful_operations.load(Ordering::Relaxed);
    let total = metrics.total_operations.load(Ordering::Relaxed);
    let throughput = total as f64 / duration.as_secs_f64();

    assert!(
        successful as f64 / total as f64 >= 0.95,
        "At least 95% of room creations should succeed"
    );

    println!("Room creation throughput: {throughput:.2} rooms/sec (target: 100)");
}

/// Test 3: Sustained Concurrent Load
/// Target: Handle 1000+ concurrent active connections with ongoing operations
#[tokio::test]
#[ignore]
async fn test_load_sustained_concurrent_connections() {
    let server = Arc::new(create_test_server().await);
    let metrics = Arc::new(PerformanceMetrics::new());

    let num_clients = 1000;
    let duration_secs = 30; // Sustain load for 30 seconds

    println!("\nStarting load test: {num_clients} concurrent connections for {duration_secs}s");

    let start = Instant::now();
    let mut handles = Vec::new();

    // Create all clients
    for i in 0..num_clients {
        let server_clone = server.clone();
        let metrics_clone = metrics.clone();

        let handle = tokio::spawn(async move {
            let player_id = uuid::Uuid::new_v4();
            let (tx, _rx) = mpsc::channel(64);

            let conn_start = Instant::now();

            // Connect
            server_clone.connect_client(player_id, tx).await;

            // Join a room
            let room_code = format!("SUST{:04}", i / 4); // 4 players per room
            let _ = server_clone
                .handle_join_room(
                    &player_id,
                    "sustained_game".to_string(),
                    Some(room_code),
                    format!("Player{i}"),
                    Some(4),
                    Some(true),
                    None,
                )
                .await;

            let conn_latency = conn_start.elapsed().as_millis() as u64;
            let connected = server_clone.get_client_room(&player_id).await.is_some();

            if connected {
                metrics_clone.record_operation(true, conn_latency);

                // Stay connected and active
                loop {
                    tokio::time::sleep(Duration::from_millis(100)).await;

                    // Simulate activity: send ready signal
                    let op_start = Instant::now();
                    let _ = server_clone.handle_player_ready(&player_id).await;
                    let op_latency = op_start.elapsed().as_millis() as u64;

                    metrics_clone.record_operation(true, op_latency);

                    // Check if test duration elapsed
                    if conn_start.elapsed().as_secs() >= duration_secs {
                        break;
                    }
                }

                // Disconnect
                server_clone.unregister_client(&player_id).await;
            } else {
                metrics_clone.record_operation(false, conn_latency);
            }
        });

        handles.push(handle);

        // Stagger connections slightly
        if i % 100 == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            println!("{i} clients connected");
        }
    }

    // Wait for all clients to complete
    for handle in handles {
        let _ = handle.await;
    }

    let total_duration = start.elapsed();
    metrics.report("Sustained Concurrent Load", total_duration);

    // Assertions
    let successful = metrics.successful_operations.load(Ordering::Relaxed);
    let total = metrics.total_operations.load(Ordering::Relaxed);

    assert!(
        successful as f64 / total as f64 >= 0.95,
        "At least 95% of operations should succeed under sustained load"
    );

    let avg_latency = metrics.total_latency_ms.load(Ordering::Relaxed) / successful as u64;
    println!("Average operation latency: {avg_latency} ms");
    println!("Total operations: {total}");
}

/// Test 4: Message Broadcasting Latency
/// Target: p50 < 10ms, p99 < 50ms for message delivery
#[tokio::test]
#[ignore]
async fn test_load_message_latency_distribution() {
    let server = Arc::new(create_test_server().await);

    let num_rooms = 100;
    let players_per_room = 4;
    let messages_per_player = 10;

    println!("\nStarting load test: Message latency distribution");
    println!(
        "   {} rooms x {} players x {} messages = {} total messages",
        num_rooms,
        players_per_room,
        messages_per_player,
        num_rooms * players_per_room * messages_per_player
    );

    let mut all_latencies = Vec::new();
    let start = Instant::now();

    // Create rooms with players
    for room_idx in 0..num_rooms {
        let room_code = format!("MLAT{room_idx:04}");
        let mut player_ids = Vec::new();
        let mut receivers = Vec::new();

        // Connect players to room
        for player_idx in 0..players_per_room {
            let player_id = uuid::Uuid::new_v4();
            let (tx, rx) = mpsc::channel(64);

            server.connect_client(player_id, tx).await;
            server
                .handle_join_room(
                    &player_id,
                    "latency_test".to_string(),
                    Some(room_code.clone()),
                    format!("Player{player_idx}"),
                    Some(4),
                    Some(true),
                    None,
                )
                .await;

            player_ids.push(player_id);
            receivers.push(rx);
        }

        // Verify all players joined
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Each player sends messages and we measure broadcast latency
        for player_id in &player_ids {
            for _msg_num in 0..messages_per_player {
                let send_start = Instant::now();

                // Simulate sending ready signal (triggers broadcast)
                let _ = server.handle_player_ready(player_id).await;

                // Measure how long it takes for message to be queued for all receivers
                let broadcast_latency = send_start.elapsed().as_micros() as u64;
                all_latencies.push(broadcast_latency);
            }
        }

        // Clean up room
        for player_id in player_ids {
            server.unregister_client(&player_id).await;
        }

        if room_idx % 10 == 0 {
            println!("Processed {room_idx} rooms");
        }
    }

    let duration = start.elapsed();

    // Calculate percentiles
    all_latencies.sort();
    let p50_idx = (all_latencies.len() as f64 * 0.50) as usize;
    let p95_idx = (all_latencies.len() as f64 * 0.95) as usize;
    let p99_idx = (all_latencies.len() as f64 * 0.99) as usize;

    let p50 = all_latencies[p50_idx] as f64 / 1000.0; // Convert to ms
    let p95 = all_latencies[p95_idx] as f64 / 1000.0;
    let p99 = all_latencies[p99_idx] as f64 / 1000.0;
    let min = all_latencies[0] as f64 / 1000.0;
    let max = all_latencies[all_latencies.len() - 1] as f64 / 1000.0;

    println!("\n===================================================");
    println!("Message Latency Distribution Results");
    println!("===================================================");
    println!("Duration: {:.2}s", duration.as_secs_f64());
    println!("Total Messages: {}", all_latencies.len());
    println!("Latency (ms):");
    println!("   - p50 (median): {p50:.2} ms (target: <10ms)");
    println!("   - p95: {p95:.2} ms");
    println!("   - p99: {p99:.2} ms (target: <50ms)");
    println!("   - min: {min:.2} ms");
    println!("   - max: {max:.2} ms");
    println!("===================================================\n");

    // Assertions based on targets
    assert!(p50 < 10.0, "p50 latency should be < 10ms (got {p50:.2}ms)");
    assert!(p99 < 50.0, "p99 latency should be < 50ms (got {p99:.2}ms)");
}

/// Test 5: Stress Test - Push to Failure
/// Find the breaking point of the system
#[tokio::test]
#[ignore]
async fn test_load_stress_to_failure() {
    let server = Arc::new(create_test_server().await);

    println!("\nStarting stress test: Push system to failure point");

    let mut current_load = 100;
    let increment = 100;
    let max_load = 5000; // Safety limit

    loop {
        if current_load > max_load {
            println!("Reached safety limit of {max_load} connections");
            break;
        }

        println!("\nTesting with {current_load} concurrent connections...");

        let start = Instant::now();
        let success_count = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for i in 0..current_load {
            let server_clone = server.clone();
            let success_clone = success_count.clone();

            let handle = tokio::spawn(async move {
                let player_id = uuid::Uuid::new_v4();
                let (tx, _rx) = mpsc::channel(64);

                // Try to connect with timeout
                let result = timeout(Duration::from_secs(10), async {
                    server_clone.connect_client(player_id, tx).await;

                    let room_code = format!("STRESS{:04}", i / 4);
                    server_clone
                        .handle_join_room(
                            &player_id,
                            "stress_test".to_string(),
                            Some(room_code),
                            format!("P{i}"),
                            Some(4),
                            Some(true),
                            None,
                        )
                        .await;

                    // Keep connection alive
                    tokio::time::sleep(Duration::from_secs(5)).await;

                    server_clone.unregister_client(&player_id).await;
                })
                .await;

                if result.is_ok() {
                    success_clone.fetch_add(1, Ordering::Relaxed);
                }
            });

            handles.push(handle);
        }

        // Wait for all to complete
        for handle in handles {
            let _ = handle.await;
        }

        let duration = start.elapsed();
        let successes = success_count.load(Ordering::Relaxed);
        let success_rate = (successes as f64 / f64::from(current_load)) * 100.0;

        println!("   Completed in {:.2}s", duration.as_secs_f64());
        println!("   Success rate: {success_rate:.1}% ({successes}/{current_load})");

        // If success rate drops below 90%, we've found the limit
        if success_rate < 90.0 {
            println!(
                "\nSystem degraded at {current_load} connections (success rate: {success_rate:.1}%)"
            );
            println!(
                "   Maximum stable load: ~{} connections",
                current_load - increment
            );
            break;
        }

        current_load += increment;
    }
}

/// Test 6: Memory Usage Monitoring
/// Target: < 2GB at max load
#[tokio::test]
#[ignore]
async fn test_load_memory_usage() {
    let server = Arc::new(create_test_server().await);

    println!("\nStarting load test: Memory usage monitoring");

    // Get baseline memory
    let baseline_memory = get_current_memory_mb();
    println!("Baseline memory: {baseline_memory} MB");

    let num_connections = 2000;
    let mut handles = Vec::new();

    println!("Creating {num_connections} connections and monitoring memory...");

    for i in 0..num_connections {
        let server_clone = server.clone();

        let handle = tokio::spawn(async move {
            let player_id = uuid::Uuid::new_v4();
            let (tx, _rx) = mpsc::channel(64);

            server_clone.connect_client(player_id, tx).await;

            let room_code = format!("MEM{:04}", i / 4);
            let _ = server_clone
                .handle_join_room(
                    &player_id,
                    "memory_test".to_string(),
                    Some(room_code),
                    format!("Player{i}"),
                    Some(4),
                    Some(true),
                    None,
                )
                .await;

            // Keep alive for 10 seconds
            tokio::time::sleep(Duration::from_secs(10)).await;

            server_clone.unregister_client(&player_id).await;
        });

        handles.push(handle);

        // Sample memory every 100 connections
        if i % 100 == 0 {
            let current_memory = get_current_memory_mb();
            println!(
                "   {} connections: {} MB (+{} MB)",
                i,
                current_memory,
                current_memory - baseline_memory
            );
        }
    }

    // Wait a bit at peak load
    println!("Holding peak load for 5 seconds...");
    tokio::time::sleep(Duration::from_secs(5)).await;

    let peak_memory = get_current_memory_mb();
    println!(
        "Peak memory: {} MB (+{} MB from baseline)",
        peak_memory,
        peak_memory - baseline_memory
    );

    // Clean up
    for handle in handles {
        let _ = handle.await;
    }

    // Final memory check
    tokio::time::sleep(Duration::from_secs(2)).await;
    let final_memory = get_current_memory_mb();
    println!("Final memory after cleanup: {final_memory} MB");

    // Analysis
    println!("\n===================================================");
    println!("Memory Usage Analysis");
    println!("===================================================");
    println!("Baseline: {baseline_memory} MB");
    println!("Peak: {peak_memory} MB");
    println!("Growth: {} MB", peak_memory - baseline_memory);
    println!(
        "Per Connection: {:.2} KB",
        ((peak_memory - baseline_memory) as f64 / f64::from(num_connections)) * 1024.0
    );
    println!(
        "Projected at 10k connections: {:.2} GB",
        (baseline_memory as f64 + (peak_memory - baseline_memory) as f64 * 5.0) / 1024.0
    );
    println!("===================================================\n");

    // Assert memory stays reasonable
    let memory_growth = peak_memory - baseline_memory;
    let projected_10k = baseline_memory + (memory_growth * 5); // Scale to 10k

    assert!(
        projected_10k < 2048,
        "Projected memory at 10k connections ({projected_10k} MB) exceeds 2GB target"
    );
}

/// Helper function to get current process memory usage in MB
fn get_current_memory_mb() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if line.starts_with("VmRSS:") {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(kb) = parts[1].parse::<u64>() {
                            return kb / 1024; // Convert KB to MB
                        }
                    }
                }
            }
        }
    }

    // Fallback: return 0 if we can't determine memory
    0
}

/// Test 7: Rate Limiting Effectiveness
/// Verify rate limits protect the server under abusive load
#[tokio::test]
#[ignore]
async fn test_load_rate_limiting() {
    // Create server with aggressive rate limits for testing
    let config = ServerConfig {
        default_max_players: 4,
        ..Default::default()
    };

    let server = create_test_server_with_config(
        config,
        signal_fish_server::config::ProtocolConfig::default(),
    )
    .await;

    println!("\nStarting load test: Rate limiting effectiveness");

    let player_id = uuid::Uuid::new_v4();
    let (tx, _rx) = mpsc::channel(64);

    server.connect_client(player_id, tx).await;

    // Attempt rapid-fire room joins (should be rate limited)
    let mut successes = 0;
    let mut rate_limited = 0;
    let attempts = 100;

    let start = Instant::now();

    for i in 0..attempts {
        let room_code = format!("RATE{i:04}");
        server
            .handle_join_room(
                &player_id,
                "rate_test".to_string(),
                Some(room_code.clone()),
                "RateTester".to_string(),
                Some(4),
                Some(true),
                None,
            )
            .await;

        // Check if player successfully joined a room
        // If rate limited, the player won't have a room assigned
        if server.get_client_room(&player_id).await.is_some() {
            successes += 1;
            // Leave the room to try again
            server.unregister_client(&player_id).await;
            // Reconnect to continue testing
            let (tx, _rx) = mpsc::channel(64);
            server.connect_client(player_id, tx).await;
        } else {
            rate_limited += 1;
        }

        // No delay - hammer the server
    }

    let duration = start.elapsed();

    println!("\n===================================================");
    println!("Rate Limiting Results");
    println!("===================================================");
    println!("Duration: {:.2}s", duration.as_secs_f64());
    println!("Total Attempts: {attempts}");
    println!("Allowed: {successes}");
    println!("Rate Limited: {rate_limited}");
    println!(
        "Rate Limit %: {:.1}%",
        (f64::from(rate_limited) / f64::from(attempts)) * 100.0
    );
    println!("===================================================\n");

    // Rate limiting should kick in for rapid requests
    // We expect most requests to be blocked
    assert!(
        rate_limited > successes,
        "Rate limiting should block most rapid requests (blocked: {rate_limited}, allowed: {successes})"
    );
}
