# Skill: WebSocket Session Lifecycle Security

<!--
  trigger: session, WebSocket-auth, session-timeout, token-rotation, session-fixation, session-invalidation
  | WebSocket session creation, token delivery, JWT validation, and session timeouts
  | Security
-->

**Trigger**: When implementing WebSocket session creation, token-based auth, JWT validation,
or session timeout logic for the signaling server.

---

## When to Use

- Adding or modifying session lifecycle (creation, validation, timeout, invalidation)
- Implementing token-based auth for WebSocket handshakes
- Validating JWT tokens with explicit algorithm and audience
- Managing session-to-connection mapping for instant invalidation

## When NOT to Use

- Anti-replay mechanisms, reconnect tokens (see [websocket-session-hijacking](./websocket-session-hijacking.md))
- Rate limiting and connection caps (see [ddos-rate-limiting-connections](./ddos-rate-limiting-connections.md))
- WebSocket framing or heartbeat (see [WebSocket-protocol-patterns](./websocket-protocol-patterns.md))

## Rationalizations to Reject

| Excuse | Why It's Wrong | Required Action |
|--------|---------------|-----------------|
| "Players can't forge WebSocket messages" | Any WebSocket client can send arbitrary frames. Dev tools and custom clients bypass all client-side logic. | Validate every message server-side. Bind messages to authenticated sessions. |
| "Session fixation isn't relevant for games" | If an attacker pre-sets a session ID, they inherit the player's authenticated session after login. | Regenerate session IDs on every privilege change (login, room join, authority grant). |
| "We check Origin so we're safe from CSRF" | Origin can be absent. CORS does not apply to WebSocket upgrades. | Validate Origin AND authenticate the connection. |

---

## TL;DR

- Bind every WebSocket to an authenticated `PlayerSession` with idle (30 min) and absolute (4 hr) timeouts.
- Pass tokens via `Sec-WebSocket-Protocol` header or first message — NEVER query strings (they leak into logs).
- Use ECDSA (ES256/EdDSA), not HMAC — private key on auth server only, public key on signaling.
- Short-lived tokens (5–15 min) with JTI for revocation.

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
```

### Idle + Absolute Timeouts

```rust
const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);       // 30 min
const ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60); // 4 hr

impl PlayerSession {
    pub fn is_expired(&self) -> bool {
        let now = Utc::now();
        // BAD: only idle timeout; session lives forever if active
        // now - self.last_activity > IDLE_TIMEOUT

        // GOOD: enforce BOTH idle and absolute
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
```

---

## 2. Token Security for WebSocket Connections

### Token Delivery

```rust
// BAD: Token in query string leaks to access logs, referrer, browser history
let url = format!("ws://{}/ws?token={}", host, token);

// GOOD: Token in Sec-WebSocket-Protocol header
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
```

### JWT Validation — Explicit Everything

```rust
// BAD: Lets the token choose its own algorithm (alg:none attack)
let data = decode::<GameClaims>(token, &key, &Validation::default())?;

// GOOD: Explicit algorithm, issuer, audience, required claims
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

// BAD: HMAC — shared secret; if it leaks, attacker forges tokens for ALL services
let key = EncodingKey::from_secret(b"shared-across-services");

// GOOD: ECDSA — private key on auth server only, public key on signaling
let decoding_key = DecodingKey::from_ec_pem(PUBLIC_KEY_PEM)?;
```

---

## Agent Checklist

- [ ] `PlayerSession` tracks session_id, player_id, created_at, last_activity, ip_address
- [ ] Idle timeout (30 min) and absolute timeout (4 hr) both enforced
- [ ] Session invalidation closes ALL WebSocket connections for that user immediately
- [ ] Tokens passed via `Sec-WebSocket-Protocol` or first message — never query strings
- [ ] Handshake tokens are short-lived (5–15 min) with JTI for revocation
- [ ] JWT validation uses explicit algorithm (ES256/EdDSA), issuer, audience, required claims
- [ ] Token denylist stores JTI hashes; auto-purges at original expiry

---

## Related Skills

- [websocket-session-hijacking](./websocket-session-hijacking.md) — Session hijacking, CSWSH, anti-replay, reconnect tokens
- [web-service-security-auth](./web-service-security-auth.md) — General auth patterns, input validation, TLS
- [WebSocket-protocol-patterns](./websocket-protocol-patterns.md) — WebSocket lifecycle, message framing, heartbeat
