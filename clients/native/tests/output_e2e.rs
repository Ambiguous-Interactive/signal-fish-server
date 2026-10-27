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
async fn unread_stderr_preserves_current_thread_protocol_failure() {
    blocked_diagnostic_output("current", false).await;
}

#[tokio::test]
async fn unread_stderr_preserves_multi_thread_protocol_failure() {
    blocked_diagnostic_output("multi", false).await;
}

#[tokio::test]
async fn closed_stderr_preserves_both_runtime_protocol_failures() {
    for runtime in ["current", "multi"] {
        blocked_diagnostic_output(runtime, true).await;
    }
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
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
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
    let held_stdout = (!close_output).then_some(stdout);
    release.send(()).unwrap();
    let status = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
    if status.is_err() {
        child.kill().await.unwrap();
    }
    drop(held_stdout);
    tokio::time::timeout(Duration::from_secs(5), stub)
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.is_ok(),
        "{runtime}: unread or closed stdout must not strand the process"
    );
    assert_eq!(
        status.unwrap().unwrap().code(),
        Some(if close_output { 3 } else { 4 })
    );
}

#[tokio::test]
async fn unread_stdout_preserves_current_thread_watchdog() {
    blocked_handshake_output("current", false).await;
}

#[tokio::test]
async fn unread_stdout_preserves_multi_thread_watchdog() {
    blocked_handshake_output("multi", false).await;
}

#[tokio::test]
async fn closed_stdout_fails_held_handshake() {
    blocked_handshake_output("current", true).await;
}

#[tokio::test]
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
async fn unread_stderr_diagnostic_flood_preserves_current_thread_watchdog() {
    diagnostic_flood_preserves_watchdog("current").await;
}

#[tokio::test]
async fn unread_stderr_diagnostic_flood_preserves_multi_thread_watchdog() {
    diagnostic_flood_preserves_watchdog("multi").await;
}

#[tokio::test]
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
