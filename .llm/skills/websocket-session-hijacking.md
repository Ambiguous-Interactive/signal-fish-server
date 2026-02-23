# Skill: WebSocket Session Hijacking Prevention

<!--
  trigger: hijack, replay, csrf, cswsh, peer-identity, session-hijacking, reconnect-token, nonce, sequence-number
  | Session hijacking prevention, CSWSH, anti-replay, reconnect tokens, peer identity
  | Security
-->

**Trigger**: When implementing CSWSH defenses, anti-replay mechanisms, reconnect token security,
or peer identity verification for the signaling server.

---

## When to Use

- Handling reconnection flows with cryptographic tokens
- Validating peer identity in signaling messages
- Adding anti-replay protection for critical signaling operations
- Reviewing Origin validation and CSWSH defenses
- Binding sessions to IP + User-Agent

## When NOT to Use

- Session lifecycle and JWT token validation (see [WebSocket-session-lifecycle](./websocket-session-lifecycle.md))
- Rate limiting and connection caps (see [ddos-rate-limiting-connections](./ddos-rate-limiting-connections.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "We check Origin so we're safe from CSRF" | Origin can be absent. CORS does not apply to WebSocket upgrades — browsers send cookies without preflight. | Validate Origin AND authenticate the connection. |
| "Replay attacks don't matter for signaling" | Replaying a room-creation or authority-transfer message can hijack game state. | Add sequence numbers and nonces to critical operations. |
| "Short sessions aren't worth protecting" | A 30-second reconnect window is enough to hijack a competitive match. | Use cryptographic one-time tokens with IP binding and short TTL. |

---

## TL;DR

- Validate `Origin` against an explicit allowlist on every upgrade — CORS does not protect WebSockets.
- Attach sequence numbers to all messages and nonces to critical operations to prevent replay.
- Use cryptographic one-time reconnect tokens (HMAC-SHA256, 30–300s TTL) bound to IP range.
- Never trust client-claimed identity — always use the session's authenticated player ID.

---

## 3. Session Hijacking Prevention

### Bind Sessions to IP + User-Agent

```rust
impl PlayerSession {
    fn validate_fingerprint(&self, ip: IpAddr, ua: &str) -> Result<(), SecurityEvent> {
        if self.ip_address != ip {
            tracing::warn!(session = %self.session_id, %ip, "IP changed mid-session");
            return Err(SecurityEvent::IpChanged);
        }
        if self.user_agent != ua {
            tracing::warn!(session = %self.session_id, "User-Agent changed mid-session");
            return Err(SecurityEvent::UaChanged);
        }
        Ok(())
    }
}
```

### Session Fixation Protection

```rust
// BAD: Same session ID after privilege change
async fn join_room(&mut self, room: &RoomId) { self.room = Some(room.clone()); }

// GOOD: Regenerate session ID on privilege change
async fn join_room(&mut self, room: &RoomId, registry: &SessionRegistry) {
    let old = std::mem::replace(&mut self.session_id, SessionId::generate());
    self.room = Some(room.clone());
    registry.migrate(old, self.session_id.clone()).await;
}
```

### Constant-Time Comparison + Zeroize

```rust
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

// BAD: Timing side-channel
fn verify(a: &str, b: &str) -> bool { a == b }

// GOOD: Constant-time
fn verify(a: &[u8], b: &[u8]) -> bool { a.len() == b.len() && a.ct_eq(b).into() }

// Zeroize secrets on drop
struct TokenSecret { key: Vec<u8> }
impl Drop for TokenSecret { fn drop(&mut self) { self.key.zeroize(); } }
```

---

## 4. Cross-Site WebSocket Hijacking (CSWSH)

CORS does **not** protect WebSocket upgrades. Browsers send cookies on cross-origin
`new WebSocket(...)` without preflight. You MUST validate Origin.

### Origin Allowlist

```rust
fn validate_origin(headers: &HeaderMap, allowed: &[String]) -> Result<(), StatusCode> {
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        // BAD: Silently accepting missing Origin
        // None => Ok(()),

        // GOOD: Reject without Origin
        None => {
            tracing::warn!("WS upgrade rejected: missing Origin");
            Err(StatusCode::FORBIDDEN)
        }
        Some(origin) if allowed.iter().any(|a| a == origin) => Ok(()),
        Some(origin) => {
            tracing::warn!(%origin, "WS upgrade rejected: Origin not in allowlist");
            Err(StatusCode::FORBIDDEN)
        }
    }
}
```

---

## 5. Anti-Replay for Signaling Messages

### Sequence Numbers

```rust
struct PeerState { expected_seq: AtomicU64 }
impl PeerState {
    fn validate_seq(&self, received: u64) -> Result<(), ProtocolError> {
        let expected = self.expected_seq.load(Ordering::Acquire);
        if received != expected {
            return Err(ProtocolError::InvalidSequence { expected, received });
        }
        self.expected_seq.store(expected + 1, Ordering::Release);
        Ok(())
    }
}
```

### Nonce-Based Prevention for Critical Operations

```rust
struct NonceRegistry { seen: DashSet<String> }
impl NonceRegistry {
    fn check_and_consume(&self, nonce: &str) -> Result<(), ProtocolError> {
        if !self.seen.insert(nonce.to_string()) { return Err(ProtocolError::ReplayedNonce); }
        Ok(())
    }
}

// Critical operations require nonces; routine messages use sequence numbers
#[derive(serde::Deserialize)]
#[serde(tag = "type")]
enum SignalingMessage {
    Signal { target: PeerId, sdp: String, seq: u64 },
    CreateRoom { room: RoomCode, nonce: String, seq: u64 },     // nonce required
    TransferAuthority { to: PeerId, nonce: String, seq: u64 },  // nonce required
}
```

Replay scenarios: room-creation replay exhausts limits; authority-transfer replay steals host; SDP replay enables MITM.

---

## 6. Reconnection Token Security

### Cryptographic Reconnect Tokens (HMAC-SHA256)

```rust
fn generate_reconnect_token(session: &PlayerSession, secret: &[u8]) -> String {
    let payload = format!("{}:{}:{}", session.session_id, session.player_id, Utc::now().timestamp());
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
    mac.update(payload.as_bytes());
    format!("{}.{}", base64_url::encode(payload.as_bytes()), base64_url::encode(&mac.finalize().into_bytes()))
}
```

### One-Time Use (Atomic CAS) + Short TTL + IP Binding

```rust
struct ReconnectToken {
    session_id: SessionId,
    created_at: DateTime<Utc>,
    ip_prefix: IpNet,  // /24 IPv4, /48 IPv6
    used: AtomicBool,
}
const TTL_CASUAL: Duration = Duration::from_secs(300);    // 5 min
const TTL_COMPETITIVE: Duration = Duration::from_secs(30); // 30 sec

impl ReconnectToken {
    fn validate(&self, client_ip: IpAddr, ttl: Duration) -> Result<(), AuthError> {
        if Utc::now() - self.created_at > ttl { return Err(AuthError::Expired); }
        if !self.ip_prefix.contains(&client_ip) { return Err(AuthError::IpMismatch); }
        // BAD: Check-then-set race: if self.used { ... } self.used = true;
        // GOOD: Atomic CAS, exactly one caller succeeds
        self.used.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AuthError::TokenReused)?;
        Ok(())
    }
}
```

---

## 7. Session Re-validation

Re-validate long-lived sessions every 30 min — check for revocation, bans, permission changes:

```rust
async fn revalidation_loop(
    session: Arc<RwLock<PlayerSession>>, action_tx: mpsc::Sender<SessionAction>,
    auth: Arc<AuthService>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(30 * 60));
    loop {
        interval.tick().await;
        let s = session.read().await;
        match auth.revalidate(&s.player_id, &s.session_id).await {
            Ok(RevalidationResult::Valid) => {}
            Ok(RevalidationResult::Revoked { reason }) => {
                let _ = action_tx.send(SessionAction::Invalidate { reason }).await;
                return;
            }
            Err(e) => tracing::error!(error = %e, "Re-validation failed"),
        }
    }
}
```

Graceful disconnect: send close reason then close frame (code 4001):

```rust
let _ = ws_tx.send(Message::Text(r#"{"type":"session_expired"}"#.into())).await;
let _ = ws_tx.send(Message::Close(Some(CloseFrame {
    code: 4001, reason: "Session expired".into(),
}))).await;
```

---

## 8. Peer Identity Verification

### Never Trust Client-Claimed Identity

```rust
async fn handle_signal(
    msg: SignalingMessage, session: &PlayerSession, state: &AppState,
) -> Result<(), ProtocolError> {
    match &msg {
        // BAD: Trusting the "from" field in the message
        // GOOD: Always use the session's authenticated player ID
        SignalingMessage::Signal { target, sdp, seq } => {
            state.rooms.verify_both_in_room(&session.player_id, target).await?;
            relay_signal(&session.player_id, target, sdp, state).await
        }
        SignalingMessage::TransferAuthority { to, nonce, seq } => {
            state.nonces.check_and_consume(nonce)?;
            let authority = state.rooms.get_authority(&session).await?;
            if authority != session.player_id {
                tracing::warn!(player = %session.player_id, "Non-authority transfer attempt");
                return Err(ProtocolError::NotAuthority);
            }
            state.rooms.transfer_authority(&session.player_id, to).await
        }
        SignalingMessage::CreateRoom { room, nonce, seq } => {
            state.nonces.check_and_consume(nonce)?;
            state.rooms.create_room_for_player(&session.player_id, room).await
        }
    }
}
```

---

## Agent Checklist

- [ ] `Origin` validated against explicit allowlist; missing Origin rejected
- [ ] Session IDs regenerated on privilege changes (login, room join, authority grant)
- [ ] Token comparison uses `subtle::ConstantTimeEq`; secrets zeroized on drop
- [ ] Sequence numbers on all messages; nonces on critical operations (CreateRoom, TransferAuthority)
- [ ] Reconnect tokens: HMAC-SHA256, one-time use (atomic CAS), IP-bound, short TTL
- [ ] Sessions re-validated every 30 min (ban/revocation check)
- [ ] Signaling uses session's player ID — never trusts client-claimed identity
- [ ] Authority transfer verified against current room authority

---

## Related Skills

- [WebSocket-session-lifecycle](./websocket-session-lifecycle.md) — Session creation, JWT validation, timeout management
- [web-service-security-auth](./web-service-security-auth.md) — General auth patterns, input validation, TLS
- [WebSocket-protocol-patterns](./websocket-protocol-patterns.md) — WebSocket lifecycle, message framing, heartbeat
- [ddos-rate-limiting-connections](./ddos-rate-limiting-connections.md) —
  Rate limiting, connection caps, abuse prevention
