# v3: Host Failover

This scenario continues from the [host topology](v3-host-topology.md) session: a running `host + webrtc` star loses
its host. The server signals the departure with `PlayerLeft`, re-elects a host over the remaining
members, and sends every survivor a fresh `SessionPlan` with the new host and fresh per-recipient ICE.

Starting state — a finalized `host + webrtc` session:

- Alice, id `00000000-0000-0000-0000-00000000000a` — the current (elected) host.
- Carol, id `00000000-0000-0000-0000-00000000000c` — a client.
- Dave, id `00000000-0000-0000-0000-00000000000d` — a client.
- Room `11111111-1111-1111-1111-111111111111`, code `ABC123`, `supports_authority: true`.

All three negotiated v3 plus the `host` topology and `webrtc` transport, so all three are electable.

## 1. The host departs

Intent: the host disconnects (an explicit `LeaveRoom`, or a dropped socket). Either way, the server removes Alice
from the room and notifies the remaining members.

If Alice left cleanly she sent:

```json
{
  "type": "LeaveRoom"
}
```

The server signals the departure to v3 survivors with `PlayerLeft` plus Alice's
terminal relay watermark:

```json
{
  "type": "PlayerLeft",
  "data": {
    "player_id": "00000000-0000-0000-0000-00000000000a",
    "epoch": 1,
    "final_seq": 0
  }
}
```

Here Alice sent no relayed game data in this incarnation, so `final_seq: 0`
lets each client retire her accounting cursor immediately. V2 recipients see
the frozen `player_id`-only projection.

Both Carol and Dave receive this `PlayerLeft`.

Next: the departed Alice was the session host, so the server detects that the stored host is now invalid and
re-elects.

## 2. The server re-elects a host

Intent: the session's topology and transport are sticky for its lifetime, so the server does **not** re-run the
selection ladder. It only re-elects a host over the members that can still run the session — those that negotiated
v3 plus the session's `host` topology and `webrtc` transport. Among them the rule is authority preferred, else
earliest joiner, with a smaller-UUID tie-break.

With Alice gone, the remaining electable members are Carol (`...000c`) and Dave (`...000d`). Neither is the room
authority now, so the tie-break falls to earliest joiner; assume Carol joined first, so **Carol is re-elected
host**.

There is no distinct "host changed" message — the re-election is communicated entirely through a fresh
`SessionPlan` to each survivor.

Next: the server mints fresh per-recipient ICE and sends each remaining v3 member a new plan.

## 3. Survivors receive a fresh SessionPlan

Intent: each remaining member receives a re-issued `SessionPlan` — same topology and transport, new `host`, fresh
ICE. The plan supersedes the previous one: clients apply the latest plan and reconnect per `peers[].initiate`.

The new host, Carol, receives a plan listing the remaining client(s) — here just Dave — with `initiate: false`
(the host answers each client):

```json
{
  "type": "SessionPlan",
  "data": {
    "topology": "host",
    "transport": "webrtc",
    "host": "00000000-0000-0000-0000-00000000000c",
    "peers": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000d",
        "player_name": "Dave",
        "is_authority": false,
        "initiate": false
      }
    ],
    "ice_servers": [
      { "urls": ["stun:stun.l.google.com:19302"] },
      {
        "urls": ["turn:turn.example.com:3478"],
        "username": "1700009900:00000000-0000-0000-0000-00000000000c",
        "credential": "Cf9aZ8b7Yc6Xd5We4Vf3Ug2Th1Si0Rj="
      }
    ],
    "fallback": "relay"
  }
}
```

The remaining client, Dave, receives a plan whose single peer is the **new** host Carol, `initiate: true`,
`is_authority: true`:

```json
{
  "type": "SessionPlan",
  "data": {
    "topology": "host",
    "transport": "webrtc",
    "host": "00000000-0000-0000-0000-00000000000c",
    "peers": [
      {
        "player_id": "00000000-0000-0000-0000-00000000000c",
        "player_name": "Carol",
        "is_authority": true,
        "initiate": true
      }
    ],
    "ice_servers": [
      { "urls": ["stun:stun.l.google.com:19302"] },
      {
        "urls": ["turn:turn.example.com:3478"],
        "username": "1700009900:00000000-0000-0000-0000-00000000000d",
        "credential": "Dg0bA9c8Zd7Ye6Xf5Wg4Vh3Ui2Tj1Sk="
      }
    ],
    "fallback": "relay"
  }
}
```

Next: Dave tears down its dead connection to the old host and offers to the new host Carol (`Signal{to: Carol,
Offer}`); Carol answers. The exchange is identical in shape to the [host topology](v3-host-topology.md) step 2,
just with Carol as the host. ICE trickle and `TransportStatus` reporting proceed as before.

## Notes

- **The relay floor never closed.** While the new WebRTC edges are being re-established, Dave and Carol can keep
  exchanging `GameData` over the relay (the universal `fallback`). Failover never interrupts the session's data
  path.
- **If no member qualifies**, no plan is re-issued — the session is over and the relay floor carries the room. For
  example, if the only other member were a relay-only seat-filler, the server would not name it host of a session
  it cannot run.
- **An ex-host that reconnects** is paired as a client of the re-elected host, because the stored host is now the
  new one. The reconnecting member learns this from the fresh `SessionPlan` it receives on entry (see the
  [reconnection scenario](reconnection.md)).
