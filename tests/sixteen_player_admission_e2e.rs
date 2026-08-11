//! P10.C H11/H12 — sixteen-player admission regression (end-to-end).
//!
//! GAP-3 / fix A3 (#140): the default per-IP connection cap was `10`, so the
//! 11th concurrent connection from one IP was refused — a 16-player session
//! behind a single NAT (LAN party, office, venue) could not assemble. A3 raised
//! `default_max_connections_per_ip` to `24`. This suite drives 16 real
//! WebSocket clients from loopback (all sharing IP `127.0.0.1`, so the per-IP
//! cap is genuinely exercised) through `Authenticate` + `JoinRoom` at the
//! **default** cap and asserts all 16 land in one room — it would fail at the
//! pre-A3 cap of 10, so it stands as the regression guard.
//!
//! A second case pins the documented room-cap trap: the per-room `max_players`
//! default is `8` (deliberately left unchanged by A3), so a room created
//! without an explicit `max_players` refuses the 9th join. Sixteen players in
//! one room therefore requires the creator to raise `max_players` at join —
//! which is exactly what the admission case does.

mod test_helpers;
mod websocket_test_helpers;

use std::collections::HashSet;
use std::sync::Arc;

use signal_fish_server::config::{defaults, ProtocolConfig};
use signal_fish_server::protocol::ErrorCode;
use signal_fish_server::server::{EnhancedGameServer, ServerConfig};
use signal_fish_server::websocket::create_router;

use test_helpers::{create_test_server_with_config, test_server_config, RunningTestServer};
use websocket_test_helpers::room16::{authenticate, connect, join_n_players, try_join};

const GAME: &str = "admission_game";
const PROTOCOL_VERSION: u16 = 4;

/// Server config at the REAL production defaults for the per-IP cap and the
/// per-room player ceiling — `test_server_config()` uses a generous cap (100)
/// and a small room default (4), neither of which would exercise the A3 fix or
/// the documented room-cap trap.
fn default_admission_config() -> ServerConfig {
    ServerConfig {
        max_connections_per_ip: defaults::default_max_connections_per_ip(),
        default_max_players: defaults::default_max_players(),
        ..test_server_config()
    }
}

/// SDK platform enforcement off, so a bare `Authenticate` (no platform) is
/// accepted — the reference/native clients this models do not claim an engine.
fn admission_protocol_config() -> ProtocolConfig {
    let mut config = ProtocolConfig::default();
    config.sdk_compatibility.enforce = false;
    config
}

async fn start_server(server: Arc<EnhancedGameServer>) -> RunningTestServer {
    let router = create_router("http://localhost:3000").with_state(server.clone());
    RunningTestServer::spawn(server, router).await
}

/// H11+H12: 16 clients from ONE IP all connect and join one room at the default
/// per-IP cap. Regression for A3 — fails at the pre-A3 cap of 10.
#[tokio::test]
async fn sixteen_players_admit_at_default_ip_cap() {
    // Premise guard: the default cap must actually admit 16 same-IP clients, or
    // this test proves nothing. If the default is ever lowered below 16 the
    // failure points straight at the cap rather than at a confusing refusal.
    let cap = defaults::default_max_connections_per_ip();
    assert!(
        cap >= 16,
        "default per-IP cap ({cap}) must admit a 16-player NAT session (GAP-3/A3)"
    );

    let server =
        create_test_server_with_config(default_admission_config(), admission_protocol_config())
            .await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    // Room created (first joiner) with max_players = 16, so the per-room ceiling
    // is not the constraint under test here — the per-IP cap is. `join_n_players`
    // panics on any refusal, so a full return already proves all 16 were admitted
    // at the default cap (the A3 regression). The assertions below add the
    // non-tautological facts: distinct identities AND actual co-location in ONE
    // room (not 16 separate rooms).
    let players = join_n_players(addr, GAME, "LOBBYA", Some(16), 16, PROTOCOL_VERSION, None).await;

    let unique: HashSet<_> = players.iter().map(|p| p.player_id).collect();
    assert_eq!(
        unique.len(),
        16,
        "the server must assign 16 distinct player ids"
    );

    // Co-location: the LAST joiner sees the whole room, so its
    // `RoomJoined.current_players` (which includes itself) must be all 16 — proof
    // that every client landed in the same room, not 16 rooms each holding one.
    let last = players.last().expect("16 players joined");
    assert_eq!(
        last.room_player_count, 16,
        "the final joiner must observe all 16 players co-located in one room"
    );
    running_server.shutdown().await;
}

/// H12: the per-room `max_players` default is 8, so a room created without an
/// explicit ceiling refuses the 9th join. Documents why the admission case must
/// raise `max_players` to seat 16.
#[tokio::test]
async fn ninth_join_fails_at_default_room_cap() {
    let default_cap = defaults::default_max_players();
    assert_eq!(
        default_cap, 8,
        "this test pins the documented default room cap of 8"
    );

    let server =
        create_test_server_with_config(default_admission_config(), admission_protocol_config())
            .await;
    let running_server = start_server(server).await;
    let addr = running_server.addr();

    // First `default_cap` joins create + fill the room (no explicit max_players,
    // so it defaults to 8); the next join must be refused as RoomFull.
    let mut seated = Vec::new();
    for i in 0..default_cap {
        let mut ws = connect(addr).await;
        authenticate(&mut ws, PROTOCOL_VERSION).await;
        let handle = try_join(ws, GAME, "LOBBYB", None, &format!("Seat{i}"))
            .await
            .unwrap_or_else(|(reason, code)| {
                panic!("player {i} within the default cap must be seated: {reason} ({code:?})")
            });
        seated.push(handle);
    }
    assert_eq!(
        u8::try_from(seated.len()).expect("default room capacity fits u8"),
        default_cap,
        "room fills to the default cap"
    );

    // The overflow join is refused, and specifically as RoomFull (not some other
    // error) — the room-cap trap, not an IP-cap or auth refusal.
    let mut overflow = connect(addr).await;
    authenticate(&mut overflow, PROTOCOL_VERSION).await;
    let error_code = match try_join(overflow, GAME, "LOBBYB", None, "Overflow").await {
        Ok(_) => panic!("the 9th join into a default-cap room must be refused"),
        Err((_reason, error_code)) => error_code,
    };
    assert_eq!(
        error_code,
        Some(ErrorCode::RoomFull),
        "overflow join must be refused as RoomFull"
    );
    running_server.shutdown().await;
}
