#[path = "fixtures/godot_reference_probe.rs"]
#[allow(dead_code)]
mod godot_reference_probe;

use std::process::Stdio;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

async fn blocked_diagnostic_output(runtime: &str, close_stderr: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let message = "界".repeat(350_000);
    let expected = message.clone();
    let stub = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.next().await.unwrap().unwrap();
        let frame = json!({"type":"AuthenticationError","data":{"error_code":"INVALID_APP_ID","error":message}});
        serde_json::from_value::<signal_fish_server::protocol::ServerMessage>(frame.clone())
            .unwrap();
        ws.send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
        while let Some(frame) = ws.next().await {
            if matches!(frame, Err(_) | Ok(Message::Close(_))) {
                break;
            }
        }
    });
    let mut child =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"))
            .args([
                "--server-url",
                &format!("ws://{addr}/v3/ws"),
                "--create-room",
                "--runtime",
                runtime,
                "--max-runtime-secs",
                "2",
            ])
            .env("RUST_LOG", "info")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let stderr = child.stderr.take().unwrap();
    let held_stderr = if close_stderr {
        drop(stderr);
        None
    } else {
        Some(stderr)
    };
    let stdout = child.stdout.take().unwrap();
    let events = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut events = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            events.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
        }
        events
    });
    let status = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
    if status.is_err() {
        child.kill().await.unwrap();
    }
    drop(held_stderr);
    let events = tokio::time::timeout(Duration::from_secs(5), events)
        .await
        .unwrap()
        .unwrap();
    let stub_result = tokio::time::timeout(Duration::from_secs(5), stub).await;
    assert!(
        matches!(stub_result, Ok(Ok(()))),
        "stub: {stub_result:?}; status: {status:?}; events: {events:?}"
    );
    assert!(
        status.is_ok(),
        "{runtime}: diagnostic output must not strand the process"
    );
    assert_eq!(status.unwrap().unwrap().code(), Some(2));
    assert!(events.iter().any(|event| event["event"] == "error"
        && event["message"]
            .as_str()
            .is_some_and(|text| text.contains(&expected))));
    assert_eq!(events.last().unwrap(), &json!({"event":"exiting","code":2}));
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn unread_stderr_preserves_current_thread_protocol_failure() {
    blocked_diagnostic_output("current", false).await;
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn unread_stderr_preserves_multi_thread_protocol_failure() {
    blocked_diagnostic_output("multi", false).await;
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn closed_stderr_preserves_both_runtime_protocol_failures() {
    for runtime in ["current", "multi"] {
        blocked_diagnostic_output(runtime, true).await;
    }
}

async fn read_child_diagnostics(mut stderr: tokio::process::ChildStderr) -> String {
    use tokio::io::AsyncReadExt;
    let mut retained = Vec::new();
    let mut chunk = [0; 4096];
    loop {
        let read = stderr
            .read(&mut chunk)
            .await
            .expect("read child diagnostics");
        if read == 0 {
            break;
        }
        let keep = read.min(16_384usize.saturating_sub(retained.len()));
        retained.extend_from_slice(&chunk[..keep]);
    }
    String::from_utf8_lossy(&retained).into_owned()
}

async fn blocked_handshake_output(runtime: &str, close_output: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (release, ready) = tokio::sync::oneshot::channel();
    let stub = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let auth = ws.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(auth.to_text().unwrap()).unwrap()["type"],
            "Authenticate"
        );
        ready.await.unwrap();
        let reply = if close_output {
            json!({"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":1000,"per_hour":60000,"per_day":1440000}}})
        } else {
            json!({"type":"Error","data":{"message":"x".repeat(1024 * 1024),"error_code":"UNSUPPORTED_GAME_DATA_FORMAT"}})
        };
        ws.send(Message::Text(reply.to_string().into()))
            .await
            .unwrap();
        while let Some(frame) = ws.next().await {
            if matches!(frame, Err(_) | Ok(Message::Close(_))) {
                break;
            }
        }
    });
    let mut child =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"))
            .args([
                "--server-url",
                &format!("ws://{addr}/v3/ws"),
                "--create-room",
                "--game-data-format",
                "message_pack",
                "--runtime",
                runtime,
                "--max-runtime-secs",
                if close_output { "90" } else { "2" },
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let diagnostics = tokio::spawn(read_child_diagnostics(child.stderr.take().unwrap()));
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = tokio::time::timeout(Duration::from_secs(5), stdout.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&first).unwrap()["event"],
        "connected"
    );
    let held_stdout = if close_output {
        drop(stdout);
        None
    } else {
        Some(stdout)
    };
    release.send(()).unwrap();
    let status = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
    if status.is_err() {
        child.kill().await.unwrap();
    }
    drop(held_stdout);
    let diagnostics = tokio::time::timeout(Duration::from_secs(5), diagnostics)
        .await
        .expect("diagnostic drain finishes after child exit")
        .expect("diagnostic reader completes");
    tokio::time::timeout(Duration::from_secs(5), stub)
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.is_ok(),
        "{runtime}: unread or closed stdout must not strand the process; diagnostics: {diagnostics}"
    );
    assert_eq!(
        status.unwrap().unwrap().code(),
        Some(if close_output { 3 } else { 4 })
    );
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn unread_stdout_preserves_current_thread_watchdog() {
    blocked_handshake_output("current", false).await;
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn unread_stdout_preserves_multi_thread_watchdog() {
    blocked_handshake_output("multi", false).await;
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn closed_stdout_fails_held_handshake() {
    blocked_handshake_output("current", true).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn spawned_stdout_reader_ownership_controls_handshake_failure() {
    use std::os::fd::{AsFd, AsRawFd};

    const CHILD_MODE: &str = "SIGNAL_FISH_STDOUT_READER_CHILD";
    if let Ok(mode) = std::env::var(CHILD_MODE) {
        use std::io::Read;
        if mode == "reader" {
            let mut stdin = std::io::stdin();
            let line = tokio::time::timeout(Duration::from_secs(5), async {
                let mut line = Vec::new();
                let mut waiting_reported = false;
                loop {
                    let mut bytes = [0; 64];
                    match stdin.read(&mut bytes) {
                        Ok(0) => panic!("client stdout closed before authenticated event"),
                        Ok(count) => {
                            line.extend_from_slice(&bytes[..count]);
                            assert!(line.len() <= 4096, "helper retains one bounded event");
                            if line.ends_with(b"\n") {
                                break line;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if !waiting_reported {
                                println!("reader waiting");
                                waiting_reported = true;
                            }
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                        Err(error) => panic!("read client stdout: {error}"),
                    }
                }
            })
            .await
            .expect("helper receives authenticated within five seconds");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&line).unwrap()["event"],
                "authenticated"
            );
            println!("reader ownership observed");
        } else {
            println!("reader ready");
        }
        tokio::time::sleep(Duration::from_secs(90)).await;
        return;
    }

    for runtime in ["current", "multi"] {
        for retain_reader in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let (release_auth, auth_ready) = tokio::sync::oneshot::channel();
            let (release_info, info_ready) = tokio::sync::oneshot::channel();
            let stub = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let auth = ws.next().await.unwrap().unwrap();
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(auth.to_text().unwrap()).unwrap()
                        ["type"],
                    "Authenticate"
                );
                auth_ready.await.unwrap();
                let auth = json!({"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":1000,"per_hour":60000,"per_day":1440000}}});
                ws.send(Message::Text(auth.to_string().into()))
                    .await
                    .unwrap();
                info_ready.await.unwrap();
                let info = json!({"type":"ProtocolInfo","data":{"protocol_version":3,"sdk_version":"test","capabilities":[],"game_data_formats":["json"]}});
                if ws
                    .send(Message::Text(info.to_string().into()))
                    .await
                    .is_ok()
                {
                    while let Some(frame) = ws.next().await {
                        if matches!(frame, Err(_) | Ok(Message::Close(_))) {
                            break;
                        }
                    }
                }
            });
            let mut child =
                tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"))
                    .args([
                        "--server-url",
                        &format!("ws://{addr}/v3/ws"),
                        "--create-room",
                        "--runtime",
                        runtime,
                        "--max-runtime-secs",
                        "90",
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap();
            let diagnostics = tokio::spawn(read_child_diagnostics(child.stderr.take().unwrap()));
            let stdout = child.stdout.take().unwrap();
            let descriptor = stdout.as_raw_fd();
            let pipe = std::fs::read_link(format!("/proc/self/fd/{descriptor}")).unwrap();
            let descriptor_info =
                std::fs::read_to_string(format!("/proc/self/fdinfo/{descriptor}")).unwrap();
            let flags = descriptor_info
                .lines()
                .find_map(|line| line.strip_prefix("flags:\t"))
                .unwrap();
            assert_ne!(
                u32::from_str_radix(flags, 8).unwrap() & 0o2000000,
                0,
                "stdout reader must close across exec: {descriptor_info}"
            );
            let sibling_stdin = if retain_reader {
                Stdio::from(stdout.as_fd().try_clone_to_owned().unwrap())
            } else {
                Stdio::null()
            };
            let mut stdout = BufReader::new(stdout).lines();
            let connected = tokio::time::timeout(Duration::from_secs(5), stdout.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&connected).unwrap()["event"],
                "connected"
            );
            let mut sibling = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "spawned_stdout_reader_ownership_controls_handshake_failure",
                    "--nocapture",
                ])
                .env(
                    CHILD_MODE,
                    if retain_reader { "reader" } else { "ordinary" },
                )
                .stdin(sibling_stdin)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let sibling_id = sibling.id().unwrap();
            let sibling_pipe_descriptors: Vec<_> =
                std::fs::read_dir(format!("/proc/{sibling_id}/fd"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| std::fs::read_link(path).is_ok_and(|target| target == pipe))
                    .collect();
            assert_eq!(sibling_pipe_descriptors.len(), usize::from(retain_reader), "{runtime}, retained={retain_reader}: {sibling_pipe_descriptors:?}; {descriptor_info}");
            drop(stdout);
            let mut sibling_output = BufReader::new(sibling.stdout.take().unwrap()).lines();
            tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(line) = sibling_output.next_line().await.unwrap() {
                    if line.ends_with(if retain_reader {
                        "reader waiting"
                    } else {
                        "reader ready"
                    }) {
                        return;
                    }
                }
                panic!("sibling must report its reader state");
            })
            .await
            .unwrap();
            release_auth.send(()).unwrap();
            if retain_reader {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while let Some(line) = sibling_output.next_line().await.unwrap() {
                        if line.ends_with("reader ownership observed") {
                            return;
                        }
                    }
                    panic!("sibling must receive the exact authenticated event");
                })
                .await
                .unwrap();
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), child.wait())
                        .await
                        .is_err(),
                    "{runtime}: the sibling reader must mask the closed original reader"
                );
                sibling.kill().await.unwrap();
            }
            release_info.send(()).unwrap();
            let status = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
            if status.is_err() {
                child.kill().await.unwrap();
            }
            if !retain_reader {
                assert!(
                    sibling.try_wait().unwrap().is_none(),
                    "ordinary sibling must remain alive during failed client exit"
                );
                sibling.kill().await.unwrap();
            }
            let diagnostics = tokio::time::timeout(Duration::from_secs(5), diagnostics)
                .await
                .unwrap()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), stub)
                .await
                .unwrap()
                .unwrap();
            assert!(status.is_ok(), "{runtime}, retained={retain_reader}: closed stdout must finish within eight seconds; diagnostics: {diagnostics}; sibling descriptors: {sibling_pipe_descriptors:?}; {descriptor_info}");
            assert_eq!(status.unwrap().unwrap().code(), Some(3), "{runtime}, retained={retain_reader}: {diagnostics}; sibling descriptors: {sibling_pipe_descriptors:?}; {descriptor_info}");
            eprintln!("{runtime}, retained={retain_reader}: code=3; sibling descriptors: {sibling_pipe_descriptors:?}; {descriptor_info}");
        }
    }
}

/// Check writer failure with an explicit retained-reader observation window.
async fn retained_stdout_reader_control(held_window: Duration) {
    async fn read_event(reader: std::io::PipeReader) -> (serde_json::Value, std::io::PipeReader) {
        tokio::time::timeout(
            Duration::from_secs(5),
            tokio::task::spawn_blocking(move || {
                use std::io::BufRead;
                let mut reader = std::io::BufReader::new(reader);
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                (serde_json::from_str(&line).unwrap(), reader.into_inner())
            }),
        )
        .await
        .expect("child emits the controlled event")
        .expect("pipe reader completes")
    }

    for runtime in ["current", "multi"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (release_auth, auth_ready) = tokio::sync::oneshot::channel();
        let (release_info, info_ready) = tokio::sync::oneshot::channel();
        let stub = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let auth = ws.next().await.unwrap().unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(auth.to_text().unwrap()).unwrap()["type"],
                "Authenticate"
            );
            auth_ready.await.unwrap();
            let reply = json!({"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":1000,"per_hour":60000,"per_day":1440000}}});
            ws.send(Message::Text(reply.to_string().into()))
                .await
                .unwrap();
            info_ready.await.unwrap();
            let reply = json!({"type":"ProtocolInfo","data":{"protocol_version":3,"sdk_version":"test","capabilities":[],"game_data_formats":["json"]}});
            if ws
                .send(Message::Text(reply.to_string().into()))
                .await
                .is_ok()
            {
                while let Some(frame) = ws.next().await {
                    if matches!(frame, Err(_) | Ok(Message::Close(_))) {
                        break;
                    }
                }
            }
        });
        let (reader, writer) = std::io::pipe().unwrap();
        let extra_reader = reader.try_clone().unwrap();
        let mut child =
            tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"))
                .args([
                    "--server-url",
                    &format!("ws://{addr}/v3/ws"),
                    "--create-room",
                    "--runtime",
                    runtime,
                    "--max-runtime-secs",
                    "90",
                ])
                .stdout(writer)
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
        let diagnostics = tokio::spawn(read_child_diagnostics(child.stderr.take().unwrap()));
        let (connected, reader) = read_event(reader).await;
        assert_eq!(connected["event"], "connected");
        drop(reader);
        release_auth.send(()).unwrap();
        let (authenticated, extra_reader) = read_event(extra_reader).await;
        assert_eq!(authenticated["event"], "authenticated");
        let held_status = tokio::time::timeout(held_window, child.wait()).await;
        drop(extra_reader);
        release_info.send(()).unwrap();
        let status = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
        if status.is_err() {
            child.kill().await.unwrap();
        }
        let diagnostics = tokio::time::timeout(Duration::from_secs(5), diagnostics)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), stub)
            .await
            .unwrap()
            .unwrap();
        assert!(held_status.is_err(), "{runtime}: retained reader must keep output writable: {held_status:?}; diagnostics: {diagnostics}");
        assert!(status.is_ok(), "{runtime}: final reader closure must fail the next write: {status:?}; diagnostics: {diagnostics}");
        assert_eq!(
            status.unwrap().unwrap().code(),
            Some(3),
            "{runtime}: {diagnostics}"
        );
    }
}

/// A second reader keeps stdout writable until it closes and another event is written.
#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn retained_stdout_reader_defers_failure_until_the_next_output_write() {
    retained_stdout_reader_control(Duration::from_millis(100)).await;
}

/// Qualify the exact historical non-exit window separately from routine liveness checks.
#[tokio::test]
#[ignore = "eight-second retained-reader qualification in both runtimes"]
#[serial_test::serial(native_output_process)]
async fn retained_stdout_reader_survives_the_original_exit_window() {
    retained_stdout_reader_control(Duration::from_secs(8)).await;
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn unread_stdout_fails_streaming_relay_before_the_watchdog() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (release, ready) = tokio::sync::oneshot::channel();
    let stub = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        ws.next().await.unwrap().unwrap();
        let me = "00000000-0000-0000-0000-000000000001";
        let peer = "00000000-0000-0000-0000-000000000002";
        let auth = json!({"type":"Authenticated","data":{"app_name":"test","rate_limits":{"per_minute":1000,"per_hour":60000,"per_day":1440000}}});
        let info = json!({"type":"ProtocolInfo","data":{"protocol_version":3,"sdk_version":"0.1.0","capabilities":[],"game_data_formats":["json"]}});
        for frame in [auth, info] {
            // Validate the fixture with the production decoder before sending it.
            serde_json::from_value::<signal_fish_server::protocol::ServerMessage>(frame.clone())
                .unwrap();
            ws.send(Message::Text(frame.to_string().into()))
                .await
                .unwrap();
        }
        let join = ws.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(join.to_text().unwrap()).unwrap()["type"],
            "JoinRoom"
        );
        let baseline = json!({"type":"RoomJoined","data":{
            "player_id":me,"room_id":me,"room_code":"TEST","game_name":"test","max_players":2,"supports_authority":false,"is_authority":false,"relay_type":"websocket","lobby_state":"lobby","ready_players":[],
            "current_players":[{"id":me,"name":"me","is_authority":false,"is_ready":false,"epoch":1,"seq":0},{"id":peer,"name":"peer","is_authority":false,"is_ready":false,"epoch":1,"seq":0}]
        }});
        serde_json::from_value::<signal_fish_server::protocol::ServerMessage>(baseline.clone())
            .unwrap();
        ws.send(Message::Text(baseline.to_string().into()))
            .await
            .unwrap();
        ready.await.unwrap();
        for seq in 1..=512 {
            let frame = json!({"type":"GameData","data":{"from_player":peer,"epoch":1,"seq":seq,"data":"x".repeat(64 * 1024)}});
            if ws
                .send(Message::Text(frame.to_string().into()))
                .await
                .is_err()
            {
                break;
            }
        }
        while let Some(frame) = ws.next().await {
            if matches!(frame, Err(_) | Ok(Message::Close(_))) {
                break;
            }
        }
    });
    let workdir = tempfile::tempdir().unwrap();
    let stderr = std::fs::File::create(workdir.path().join("stderr.log")).unwrap();
    let mut child =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"))
            .args([
                "--server-url",
                &format!("ws://{addr}/v3/ws"),
                "--create-room",
                "--peers",
                "2",
                "--runtime",
                "current",
                "--max-runtime-secs",
                "90",
            ])
            .stdout(Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let line = stdout
                .next_line()
                .await
                .unwrap()
                .expect("client reaches room baseline");
            if serde_json::from_str::<serde_json::Value>(&line).unwrap()["event"] == "room_joined" {
                break;
            }
        }
    })
    .await
    .unwrap();
    release.send(()).unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    if status.is_err() {
        child.kill().await.unwrap();
    }
    drop(stdout);
    tokio::time::timeout(Duration::from_secs(5), stub)
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.is_ok(),
        "unread streaming output must fail before the watchdog"
    );
    assert_eq!(status.unwrap().unwrap().code(), Some(3));
}

async fn diagnostic_flood_preserves_watchdog(runtime: &str) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let stub = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let auth = ws.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(auth.to_text().unwrap()).unwrap()["type"],
            "Authenticate"
        );
        let (mut sink, mut source) = ws.split();
        let sender = async {
            for _ in 0..20_000 {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
        };
        let reader = async {
            let mut pongs = 0;
            while let Some(frame) = source.next().await {
                match frame {
                    Ok(Message::Pong(_)) => pongs += 1,
                    Err(_) | Ok(Message::Close(_)) => break,
                    _ => {}
                }
            }
            pongs
        };
        let ((), pongs) = tokio::join!(sender, reader);
        pongs
    });
    let mut child =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"))
            .args([
                "--server-url",
                &format!("ws://{addr}/v3/ws"),
                "--create-room",
                "--runtime",
                runtime,
                "--max-runtime-secs",
                "2",
            ])
            .env("RUST_LOG", "debug")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let events = tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut events = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            events.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
        }
        events
    });
    let status = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
    if status.is_err() {
        child.kill().await.unwrap();
    }
    drop(stderr);
    let pongs = tokio::time::timeout(Duration::from_secs(5), stub)
        .await
        .unwrap()
        .unwrap();
    let events = tokio::time::timeout(Duration::from_secs(5), events)
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.is_ok(),
        "{runtime}: diagnostic flood must not strand watchdog"
    );
    assert!(
        pongs >= 1024,
        "{runtime}: must exercise sustained diagnostic production, received {pongs} pongs"
    );
    assert_eq!(status.unwrap().unwrap().code(), Some(4));
    assert!(events.iter().any(|event| event["event"] == "error"
        && event["message"]
            .as_str()
            .is_some_and(|message| message.contains("watchdog fired"))));
    assert_eq!(events.last().unwrap(), &json!({"event":"exiting","code":4}));
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn unread_stderr_diagnostic_flood_preserves_current_thread_watchdog() {
    diagnostic_flood_preserves_watchdog("current").await;
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn unread_stderr_diagnostic_flood_preserves_multi_thread_watchdog() {
    diagnostic_flood_preserves_watchdog("multi").await;
}

#[tokio::test]
#[serial_test::serial(native_output_process)]
async fn unread_stderr_bounds_cli_errors_before_runtime_startup() {
    let mut child =
        tokio::process::Command::new(env!("CARGO_BIN_EXE_signal-fish-reference-native"))
            .args([
                "--server-url",
                "ws://127.0.0.1:1/v3/ws",
                "--create-room",
                "--max-runtime-secs",
            ])
            .arg("x".repeat(64 * 1024))
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
    let stderr = child.stderr.take().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    if status.is_err() {
        child.kill().await.unwrap();
    }
    drop(stderr);
    assert_eq!(status.unwrap().unwrap().code(), Some(2));
}
