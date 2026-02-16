# Skill: WebSocket Session Security

<!-- trigger: session, hijack, token-rotation, peer-identity, replay, csrf, cswsh, websocket-auth, session-fixation, session-timeout | WebSocket session lifecycle security for game signaling | Security -->

**Trigger**: When implementing, reviewing, or hardening WebSocket session management, token handling, reconnection security, peer identity verification, or anti-replay mechanisms in the signaling server.

---

## When to Use
- Adding or modifying session lifecycle (creation, validation, timeout, invalidation)
- Implementing token-based auth for WebSocket handshakes
- Handling reconnection flows with cryptographic tokens
- Validating peer identity in signaling messages
- Adding anti-replay protection for critical signaling operations
- Reviewing Origin validation and CSWSH defenses

## When NOT to Use
- General HTTP auth patterns without WebSocket context (see [web-service-security](./web-service-security.md))
- Rate limiting and connection caps (see [ddos-and-rate-limiting](./ddos-and-rate-limiting.md))
- WebSocket framing, protocol design, or heartbeat (see [websocket-protocol-patterns](./websocket-protocol-patterns.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "Players can't forge WebSocket messages" | Any WebSocket client can send arbitrary frames. Dev tools and custom clients bypass all client-side logic. | Validate every message server-side. Bind messages to authenticated sessions. |
| "Session fixation isn't relevant for games" | If an attacker pre-sets a session ID, they inherit the player's authenticated session after login. | Regenerate session IDs on every privilege change (login, room join, authority grant). |
| "We check Origin so we're safe from CSRF" | Origin can be absent. CORS does not apply to WebSocket upgrades — browsers send cookies without preflight. | Validate Origin AND authenticate the connection. Origin is one layer, not the only layer. |
| "Replay attacks don't matter for signaling" | Replaying a room-creation or authority-transfer message can hijack game state. | Add sequence numbers and nonces to critical operations. |
| "Short sessions aren't worth protecting" | A 30-second reconnect window is enough to hijack a competitive match. | Use cryptographic one-time tokens with IP binding and short TTL. |

---

## TL;DR

- Bind every WebSocket to an authenticated `PlayerSession` with idle (30 min) and absolute (4 hr) timeouts.
- Pass tokens via `Sec-WebSocket-Protocol` header or first message — NEVER query strings (they leak into logs).
- Validate `Origin` against an explicit allowlist on every upgrade — CORS does not protect WebSockets.
- Attach sequence numbers to all messages and nonces to critical operations to prevent replay.
- Use cryptographic one-time reconnect tokens (HMAC-SHA256, 30–300s TTL) bound to IP range.

---

## 1. Session Lifecycle Security

### PlayerSession Struct

```rust
pub struct PlayerSession {
    pub session_id: SessionId,     // 128-bit cryptographically random
    pub player_id: PlayerId,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub ip_address: IpAddr,
    pub user_agent: String,
}

pub struct SessionId(Uuid);
impl SessionId {
    pub fn generate() -> Self { Self(Uuid::new_v4()) }
}
```rust

### Idle + Absolute Timeouts

```rust
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);       // 30 min
const ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60); // 4 hr

impl PlayerSession {
    pub fn is_expired(&self) -> bool {
        let now = Utc::now();
        // ❌ Bad — only idle timeout; session lives forever if active
        // now - self.last_activity > IDLE_TIMEOUT
        // ✅ Good — enforce BOTH idle and absolute
        now - self.last_activity > IDLE_TIMEOUT || now - self.created_at > ABSOLUTE_TIMEOUT
    }
}
```

### Session-to-Connection Mapping for Instant Invalidation

On logout or ban, close ALL WebSocket connections for that user immediately:

```rust
struct SessionRegistry {
    connections: DashMap<SessionId, Vec<mpsc::Sender<SessionAction>>>,
}
enum SessionAction { Invalidate { reason: String }, Revalidate }

impl SessionRegistry {
    async fn invalidate_user(&self, player_id: &PlayerId, sessions: &[SessionId]) {
        for sid in sessions {
            if let Some((_, senders)) = self.connections.remove(sid) {
                for tx in senders {
                    let _ = tx.send(SessionAction::Invalidate {
                        reason: "User logged out".into() }).await;
                }
            }
        }
    }
}
```rust

## 2. Token Security for WebSocket Connections

### Token Delivery

```rust
// ❌ Bad — token in query string leaks to access logs, referrer, browser history
let url = format!("ws://{}/ws?token={}", host, token);

// ✅ Good — token in Sec-WebSocket-Protocol header
async fn ws_handler(
    headers: HeaderMap, ws: WebSocketUpgrade, State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    let token = headers.get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bearer."))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let claims = validate_token(token, &state.jwt_keys)?;
    Ok(ws.protocols(["bearer"])
        .on_upgrade(move |socket| handle_socket(socket, claims, state)))
}
```

### GameClaims (Short-Lived: 5–15 min)

```rust
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct GameClaims {
    pub sub: String,           // player ID
    pub app: String,           // application / game ID
    pub room: Option<String>,  // optional room scope
    pub iss: String, pub aud: String,
    pub exp: usize, pub nbf: usize, pub iat: usize,
    pub jti: String,           // unique token ID for revocation
}
```rust

### JWT Validation — Explicit Everything

```rust
// ❌ Bad — lets the token choose its own algorithm (alg:none attack)
let data = decode::<GameClaims>(token, &key, &Validation::default())?;

// ✅ Good — explicit algorithm, issuer, audience, required claims
fn validate_token(token: &str, keys: &JwtKeys) -> Result<GameClaims, AuthError> {
    let mut v = Validation::new(Algorithm::ES256); // ECDSA, not HMAC
    v.set_required_spec_claims(&["exp", "iss", "sub", "aud", "jti"]);
    v.set_issuer(&["matchbox-server"]);
    v.set_audience(&["matchbox-client"]);
    let data = decode::<GameClaims>(token, &keys.decoding, &v)?;
    if keys.denylist.contains(&data.claims.jti) { return Err(AuthError::Revoked); }
    Ok(data.claims)
}
```

### Token Denylist (JTI Hashes Until Expiry) + Algorithm Choice

```rust
struct TokenDenylist { denied: DashSet<[u8; 32]> }
impl TokenDenylist {
    fn revoke(&self, jti: &str) { self.denied.insert(Sha256::digest(jti.as_bytes()).into()); }
    fn contains(&self, jti: &str) -> bool { self.denied.contains(&Sha256::digest(jti.as_bytes()).into()) }
}
```rust

```rust
// ❌ Bad — HMAC: shared secret; if it leaks, attacker forges tokens for ALL services
let key = EncodingKey::from_secret(b"shared-across-services");
// ✅ Good — ECDSA: private key on auth server only, public key on signaling
let decoding_key = DecodingKey::from_ec_pem(PUBLIC_KEY_PEM)?;
```

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
```rust

### Session Fixation Protection

```rust
// ❌ Bad — same session ID after privilege change
async fn join_room(&mut self, room: &RoomId) { self.room = Some(room.clone()); }

// ✅ Good — regenerate session ID on privilege change
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

// ❌ Bad — timing side-channel
fn verify(a: &str, b: &str) -> bool { a == b }
// ✅ Good — constant-time
fn verify(a: &[u8], b: &[u8]) -> bool { a.len() == b.len() && a.ct_eq(b).into() }

// Zeroize secrets on drop
struct TokenSecret { key: Vec<u8> }
impl Drop for TokenSecret { fn drop(&mut self) { self.key.zeroize(); } }
```rust

## 4. Cross-Site WebSocket Hijacking (CSWSH)

CORS does **not** protect WebSocket upgrades. Browsers send cookies on cross-origin `new WebSocket(...)` without preflight. You MUST validate Origin.

### Origin Allowlist

```rust
fn validate_origin(headers: &HeaderMap, allowed: &[String]) -> Result<(), StatusCode> {
    match headers.get("origin").and_then(|v| v.to_str().ok()) {
        // ❌ Bad — silently accepting missing Origin
        // None => Ok(()),
        // ✅ Good — reject without Origin
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
            // ❌ Bad — accept any sequence number
            // ✅ Good — reject replayed or out-of-order
            return Err(ProtocolError::InvalidSequence { expected, received });
        }
        self.expected_seq.store(expected + 1, Ordering::Release);
        Ok(())
    }
}
```rust

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
```rust

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
        // ❌ Bad — check-then-set race: if self.used { ... } self.used = true;
        // ✅ Good — atomic CAS, exactly one caller succeeds
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
```rust

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
        // ❌ Bad — trusting the "from" field in the message
        // ✅ Good — always use the session's authenticated player ID
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
        _ => Ok(()),
    }
}
```rust

## Agent Checklist

- [ ] `PlayerSession` tracks session_id, player_id, created_at, last_activity, ip_address
- [ ] Idle timeout (30 min) and absolute timeout (4 hr) both enforced
- [ ] Session invalidation closes ALL WebSocket connections for that user immediately
- [ ] Tokens passed via `Sec-WebSocket-Protocol` or first message — never query strings
- [ ] Handshake tokens are short-lived (5–15 min) with JTI for revocation
- [ ] JWT validation uses explicit algorithm (ES256/EdDSA), issuer, audience, required claims
- [ ] Token denylist stores JTI hashes; auto-purges at original expiry
- [ ] `Origin` validated against explicit allowlist; missing Origin rejected
- [ ] Session IDs regenerated on privilege changes (login, room join, authority grant)
- [ ] Token comparison uses `subtle::ConstantTimeEq`; secrets zeroized on drop
- [ ] Sequence numbers on all messages; nonces on critical operations
- [ ] Reconnect tokens: HMAC-SHA256, one-time use (atomic CAS), IP-bound, short TTL
- [ ] Sessions re-validated every 30 min (ban/revocation check)
- [ ] Signaling uses session's player ID — never trusts client-claimed identity
- [ ] Authority transfer verified against current room authority

## Related Skills

- [web-service-security](./web-service-security.md) — Input validation, auth patterns, TLS, secrets management
- [websocket-protocol-patterns](./websocket-protocol-patterns.md) — WebSocket lifecycle, message framing, heartbeat
- [container-and-deployment](./container-and-deployment.md) — Infrastructure-level security, container hardening
- [ddos-and-rate-limiting](./ddos-and-rate-limiting.md) — Rate limiting, connection caps, abuse prevention
