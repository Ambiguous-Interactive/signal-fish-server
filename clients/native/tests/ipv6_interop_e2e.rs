//! IPv6 data-path interoperability over the loopback interface.
//!
//! webrtc 0.20 turns each application-supplied UDP bind directly into a host
//! candidate, so the address family the client binds decides the family of the
//! negotiated path. Every other native cell leaves `--ip-family any`, where a
//! runner that offers both families normally selects IPv4 — the IPv6 branch of
//! the bind/candidate/socket-key logic would then never be executed by any
//! live transport.
//!
//! This cell pins it: two real client processes run with `--ip-family ipv6`,
//! so an IPv6 host candidate is the only thing either side can advertise, and
//! the pair must connect and exchange the exact payload on BOTH data-channel
//! labels. The selected candidate pair is asserted to be host/host with
//! concrete, dialable IPv6 addresses on both sides.
//!
//! **Scope of the proof.** Signaling still runs over the harness server's IPv4
//! loopback listener (the server binds `0.0.0.0`); what this cell proves is the
//! WebRTC media/data path — ICE gathering, candidate wire projection, DTLS and
//! SCTP — over IPv6.
//!
//! **Prerequisite.** The runner must provide an IPv6 loopback interface. The
//! cell probes for it and fails with an actionable message rather than
//! skipping: a silent skip would report a green lane that proved nothing.

mod harness;

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv6Addr, SocketAddr, UdpSocket};

use harness::{
    events_named, player_id_of, scenario_window, single_event, spawn_client, spawn_server,
    str_field, ClientProcess, ClientSpec, CLIENT_EXIT_TIMEOUT, EVENT_TIMEOUT,
};
use serde_json::Value;

const GAME_NAME: &str = "ipv6-loopback-interop";
const CLIENT_NAMES: [&str; 2] = ["c0", "c1"];
const RELIABLE: &str = "reliable";
const UNRELIABLE: &str = "unreliable";
/// Both clients bind IPv6 only and keep mDNS obfuscation off, so every host
/// candidate is a raw IPv6 address the assertions can read.
const IPV6_ARGS: [&str; 3] = ["--ip-family", "ipv6", "--disable-mdns"];

/// Fail loudly (never skip) when the runner cannot provide IPv6 loopback.
fn require_ipv6_loopback() {
    let probe = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0);
    UdpSocket::bind(probe).unwrap_or_else(|error| {
        panic!(
            "this cell proves the IPv6 WebRTC data path and requires an IPv6 loopback \
             interface, but binding {probe} failed: {error}. Enable IPv6 on the runner \
             (Linux: `sysctl net.ipv6.conf.lo.disable_ipv6=0`; containers: run with IPv6 \
             enabled). The lane must not be skipped silently."
        )
    });
}

/// Drain one client to EOF + reap it; it must exit 0 AND report that.
async fn drain_expect_success(client: &mut ClientProcess) {
    let code = client.drain_to_exit(CLIENT_EXIT_TIMEOUT).await;
    assert_eq!(
        code,
        0,
        "client {} exited nonzero;\n{}",
        client.name,
        client.diagnostics()
    );
    assert_eq!(
        single_event(&client.events, "exiting", &client.name)
            .get("code")
            .and_then(Value::as_i64),
        Some(0),
        "client {} must report a successful exit",
        client.name
    );
}

/// The concrete IPv6 address a `selected_candidate_pair` field carries, or a
/// loud failure. A missing/`null` address is a failure: it would otherwise let
/// a run that never proved the family pass.
fn selected_ipv6_address(event: &Value, field: &str, who: &str) -> Ipv6Addr {
    let raw = event
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{who}: selected pair has no `{field}`: {event}"));
    match raw
        .parse::<IpAddr>()
        .unwrap_or_else(|error| panic!("{who}: `{field}` is not an IP address ({error}): {raw}"))
    {
        IpAddr::V6(address) => address,
        IpAddr::V4(address) => {
            panic!("{who}: the IPv6-only run selected an IPv4 candidate {address}: {event}")
        }
    }
}

/// Assert this client's single selected pair is host/host over concrete IPv6,
/// toward the expected peer.
///
/// The address is not pinned to `::1`: a runner that also has a global IPv6
/// interface binds it too, and either host candidate is a legitimate IPv6
/// proof. What must hold is that the path is IPv6, direct (host/host, since no
/// STUN or TURN is configured), and carried by an address a peer could
/// actually dial — never unspecified, multicast, or link-local, whose scope ID
/// the candidate wire cannot carry.
fn assert_ipv6_host_path(events: &[Value], who: &str, peer_id: &str) {
    let selected = single_event(events, "selected_candidate_pair", who);
    assert_eq!(
        str_field(selected, "peer"),
        peer_id,
        "{who}: the selected pair must belong to the planned peer"
    );
    for field in ["local_candidate_type", "remote_candidate_type"] {
        assert_eq!(
            str_field(selected, field),
            "host",
            "{who}: {field} must be a direct host candidate (no STUN/TURN is configured)"
        );
    }
    for field in ["local_candidate_address", "remote_candidate_address"] {
        let address = selected_ipv6_address(selected, field, who);
        assert!(
            !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_unicast_link_local(),
            "{who}: {field} must be a concrete dialable IPv6 address, got {address}"
        );
    }
}

/// Assert exactly one exchange message per label arrived from `peer_id`, with
/// the documented `{"from","channel","seq"}` payload.
fn assert_both_channels_exchanged(events: &[Value], who: &str, peer_id: &str) {
    let mut labels = BTreeSet::new();
    for event in events_named(events, "channel_message") {
        assert_eq!(
            str_field(event, "peer"),
            peer_id,
            "{who}: unexpected channel_message from a non-pair peer: {event}"
        );
        let label = str_field(event, "label").to_string();
        let text: Value = serde_json::from_str(str_field(event, "text"))
            .unwrap_or_else(|error| panic!("{who}: message text is not JSON ({error}): {event}"));
        assert_eq!(str_field(&text, "from"), peer_id, "{who}: payload sender");
        assert_eq!(str_field(&text, "channel"), label, "{who}: payload channel");
        assert_eq!(
            text.get("seq").and_then(Value::as_u64),
            Some(0),
            "{who}: payload seq"
        );
        assert!(
            labels.insert(label),
            "{who}: duplicate channel_message: {event}"
        );
    }
    assert_eq!(
        labels,
        BTreeSet::from([RELIABLE.to_string(), UNRELIABLE.to_string()]),
        "{who}: both data-channel labels must carry the exchange over IPv6"
    );
}

#[tokio::test]
async fn ipv6_loopback_mesh_pair_exchanges_on_a_host_ipv6_path() {
    require_ipv6_loopback();

    let server = spawn_server("mesh").await;
    let workdir = tempfile::tempdir().expect("create client workdir");
    let url = server.v3_ws_url();

    let mut creator = spawn_client(
        &ClientSpec {
            name: CLIENT_NAMES[0],
            server_url: &url,
            game_name: GAME_NAME,
            join_code: None,
            peers: 2,
            exchange: true,
            relay_payload: None,
            extra_args: &IPV6_ARGS,
        },
        workdir.path(),
    );
    let created = creator.await_event("room_created", EVENT_TIMEOUT).await;
    let room_code = str_field(&created, "room_code").to_string();
    let joiner = spawn_client(
        &ClientSpec {
            name: CLIENT_NAMES[1],
            server_url: &url,
            game_name: GAME_NAME,
            join_code: Some(&room_code),
            peers: 2,
            exchange: true,
            relay_payload: None,
            extra_args: &IPV6_ARGS,
        },
        workdir.path(),
    );

    let mut clients = [creator, joiner];
    for client in &mut clients {
        drain_expect_success(client).await;
    }
    let mut server = server;
    server.shutdown().await;

    let ids = [
        player_id_of(&clients[0].events, &clients[0].name),
        player_id_of(&clients[1].events, &clients[1].name),
    ];
    assert_ne!(ids[0], ids[1], "the two clients need distinct identities");

    for (index, client) in clients.iter().enumerate() {
        let who = &client.name;
        let peer_id = ids[1 - index].as_str();
        let window = scenario_window(&client.events);

        // The pair really connected over WebRTC (not the relay floor).
        let connected = events_named(window, "p2p_pair_connected");
        assert_eq!(
            connected.len(),
            1,
            "{who}: expected exactly one connected pair: {connected:?}"
        );
        assert_eq!(str_field(connected[0], "peer"), peer_id, "{who}: pair peer");
        assert!(
            events_named(&client.events, "fallback_engaged").is_empty(),
            "{who}: the IPv6 host path must carry the session, not the relay fallback;\n{}",
            client.diagnostics()
        );
        let status = single_event(window, "transport_status_sent", who);
        assert_eq!(str_field(status, "transport"), "webrtc", "{who}: transport");
        assert_eq!(
            status.get("connected").and_then(Value::as_bool),
            Some(true),
            "{who}: the reported transport status must be connected"
        );

        assert_ipv6_host_path(window, who, peer_id);
        assert_both_channels_exchanged(window, who, peer_id);
    }
}
