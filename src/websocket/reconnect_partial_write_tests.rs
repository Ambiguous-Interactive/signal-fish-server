// Controlled AsyncWrite cuts on real TCP. This does not model a particular
// kernel's write boundaries, TLS, or an application receipt acknowledgement.
#[cfg(signal_fish_repository_tests)]
mod reconnect_partial_write {
    use super::*;
    use base64::Engine as _;
    use ed25519_dalek::{Signer, SigningKey};
    use std::future::IntoFuture;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Notify;
    use tokio_tungstenite::WebSocketStream;

    #[derive(Default)]
    struct WriteCut {
        remaining: AtomicUsize,
        armed: AtomicBool,
        offered: AtomicUsize,
        parked: Notify,
    }

    struct CutStream {
        socket: TcpStream,
        cut: Arc<WriteCut>,
    }

    impl AsyncRead for CutStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.socket).poll_read(cx, buffer)
        }
    }

    impl AsyncWrite for CutStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if !self.cut.armed.load(Ordering::Acquire) {
                return Pin::new(&mut self.socket).poll_write(cx, bytes);
            }
            let remaining = self.cut.remaining.load(Ordering::Acquire);
            if remaining == 0 {
                self.cut.parked.notify_one();
                return Poll::Pending;
            }
            self.cut.offered.fetch_max(bytes.len(), Ordering::AcqRel);
            let result =
                Pin::new(&mut self.socket).poll_write(cx, &bytes[..bytes.len().min(remaining)]);
            if let Poll::Ready(Ok(written)) = &result {
                self.cut.remaining.fetch_sub(*written, Ordering::AcqRel);
            }
            result
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.socket).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.socket).poll_shutdown(cx)
        }
    }

    struct CutListener {
        listener: TcpListener,
        cut: Arc<WriteCut>,
    }

    impl axum::serve::Listener for CutListener {
        type Io = CutStream;
        type Addr = SocketAddr;

        async fn accept(&mut self) -> (Self::Io, Self::Addr) {
            let (socket, address) = self.listener.accept().await.expect("accept cut socket");
            (
                CutStream {
                    socket,
                    cut: Arc::clone(&self.cut),
                },
                address,
            )
        }

        fn local_addr(&self) -> std::io::Result<Self::Addr> {
            self.listener.local_addr()
        }
    }

    async fn send(socket: &mut WebSocketStream<TcpStream>, message: &ClientMessage) {
        tokio::time::timeout(
            Duration::from_secs(10),
            socket.send(TungsteniteMessage::Text(
                serde_json::to_string(message).unwrap().into(),
            )),
        )
        .await
        .expect("send deadline")
        .expect("send application request");
    }

    async fn receive(socket: &mut WebSocketStream<TcpStream>) -> ServerMessage {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let frame = tokio::time::timeout_at(deadline, socket.next())
                .await
                .expect("receive deadline")
                .expect("response stream")
                .expect("response frame");
            if let TungsteniteMessage::Text(text) = frame {
                return serde_json::from_str(&text).expect("decode application response");
            }
        }
    }

    async fn connect(address: SocketAddr) -> WebSocketStream<TcpStream> {
        connect_path(address, "v3/ws").await
    }

    async fn connect_path(address: SocketAddr, path: &str) -> WebSocketStream<TcpStream> {
        let stream = TcpStream::connect(address).await.expect("connect TCP");
        tokio_tungstenite::client_async(format!("ws://{address}/{path}"), stream)
            .await
            .expect("upgrade socket")
            .0
    }

    fn authentication_request(signing: &SigningKey, app_id: &str) -> ClientMessage {
        let payload = serde_json::json!({
            "app_id": app_id,
            "exp": chrono::Utc::now().timestamp() + 300,
            "nonce": "partial-write-control",
        });
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        let signed = format!("sfct_v1.{encoded}");
        let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing.sign(signed.as_bytes()).to_bytes());
        ClientMessage::Authenticate {
            app_id: app_id.into(),
            connect_token: Some(format!("{signed}.{signature}")),
            sdk_version: None,
            platform: None,
            game_data_format: None,
            protocol_version: None,
            supported_transports: None,
            supported_topologies: None,
            requested_capabilities: None,
        }
    }

    async fn authenticate(
        socket: &mut WebSocketStream<TcpStream>,
        signing: &SigningKey,
        app_id: &str,
    ) {
        send(socket, &authentication_request(signing, app_id)).await;
        assert!(matches!(
            receive(socket).await,
            ServerMessage::Authenticated { .. }
        ));
        assert!(matches!(
            receive(socket).await,
            ServerMessage::ProtocolInfo(_)
        ));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_authenticated_partial_reconnect_preserves_tenant_and_recovery() {
        run_partial_reconnect_history(true).await;
    }

    /// The writer accepts a strict frame prefix and then returns Pending.
    /// Unlike the full-frame relay control, the production write is unfinished
    /// when the peer drops. Only the v3 positive cell observes a rotated token.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_reconnected_partial_transport_write_requires_fresh_join() {
        run_partial_reconnect_history(false).await;
    }

    async fn run_partial_reconnect_history(authenticated: bool) {
        for (path, batching, prefix) in ["ws", "v3/ws"].into_iter().flat_map(|path| {
            [false, true].into_iter().flat_map(move |batching| {
                [None, Some(1_usize), Some(32)].map(|prefix| (path, batching, prefix))
            })
        }) {
            let mut config = ServerConfig::default();
            config.websocket_config.enable_batching = batching;
            config.app_id_allowlist_enabled = authenticated;
            let signing = SigningKey::from_bytes(&[42; 32]);
            let apps = if authenticated {
                ["owner-app", "other-app"]
                    .map(|app_id| crate::config::AppRegistrationEntry {
                        app_id: app_id.into(),
                        app_name: app_id.into(),
                        max_rooms: None,
                        max_players_per_room: None,
                        rate_limit_per_minute: None,
                        max_relay_bytes: None,
                        require_connect_token: Some(true),
                    })
                    .to_vec()
            } else {
                Vec::new()
            };
            let server = EnhancedGameServer::new(
                config,
                crate::config::ProtocolConfig::default(),
                crate::config::RelayTypeConfig::default(),
                crate::config::SessionConfig::default(),
                crate::config::TurnConfig::default(),
                DatabaseConfig::InMemory,
                crate::config::MetricsConfig::default(),
                crate::config::CoordinationConfig::default(),
                crate::config::TransportSecurityConfig::default(),
                apps,
            )
            .await
            .expect("construct partial-write server");
            if authenticated {
                server
                    .install_connect_token_key(&crate::config::SecurityConfig {
                        connect_token: Some(crate::config::ConnectTokenConfig {
                            public_key: base64::engine::general_purpose::STANDARD
                                .encode(signing.verifying_key()),
                            public_key_path: None,
                            required: true,
                        }),
                        ..crate::config::SecurityConfig::default()
                    })
                    .expect("install verification key");
            }
            let normal_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = normal_listener.local_addr().unwrap();
            let router =
                super::super::super::routes::create_standalone_router("http://localhost:3000")
                    .with_state(Arc::clone(&server));
            let normal_task = tokio::spawn(
                axum::serve(
                    normal_listener,
                    router
                        .clone()
                        .into_make_service_with_connect_info::<SocketAddr>(),
                )
                .into_future(),
            );
            let cut = Arc::new(WriteCut::default());
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let cut_address = listener.local_addr().unwrap();
            let cut_router = router.layer(axum::Extension(axum::extract::ConnectInfo(cut_address)));
            let cut_task = tokio::spawn(
                axum::serve(
                    CutListener {
                        listener,
                        cut: Arc::clone(&cut),
                    },
                    cut_router.into_make_service(),
                )
                .into_future(),
            );
            let history = async {
                let join = |room_code, name: &str| ClientMessage::JoinRoom {
                    game_name: "partial-reconnect".into(),
                    room_code,
                    player_name: name.into(),
                    max_players: Some(2),
                    supports_authority: Some(true),
                    relay_transport: None,
                    password: None,
                    join_only: None,
                };
                if authenticated {
                    // A pre-auth application request is terminal on this socket.
                    let mut unauthenticated = connect(address).await;
                    send(&mut unauthenticated, &join(None, "unauthenticated")).await;
                    assert!(matches!(
                        receive(&mut unauthenticated).await,
                        ServerMessage::Error {
                            error_code: Some(ErrorCode::MissingAppId),
                            ..
                        }
                    ));
                    drop(unauthenticated);
                }
                let mut incumbent = connect(address).await;
                if authenticated {
                    let rogue = SigningKey::from_bytes(&[43; 32]);
                    send(&mut incumbent, &authentication_request(&rogue, "owner-app")).await;
                    assert!(
                        matches!(
                            receive(&mut incumbent).await,
                            ServerMessage::AuthenticationError {
                                error_code: ErrorCode::ConnectTokenInvalid,
                                ..
                            }
                        ),
                        "the installed verification key must reject another signer"
                    );
                    authenticate(&mut incumbent, &signing, "owner-app").await;
                }
                send(&mut incumbent, &join(None, "incumbent")).await;
                let ServerMessage::RoomJoined(room) = receive(&mut incumbent).await else {
                    panic!("incumbent joins");
                };
                let mut original = connect(address).await;
                if authenticated {
                    authenticate(&mut original, &signing, "owner-app").await;
                }
                send(
                    &mut original,
                    &join(Some(room.room_code.clone()), "recovering"),
                )
                .await;
                let ServerMessage::RoomJoined(joined) = receive(&mut original).await else {
                    panic!("original joins");
                };
                let player_id = joined.player_id;
                let token = joined.reconnection_token.unwrap();
                let owner_app = server.client_app_id(&player_id);
                assert_eq!(owner_app.is_some(), authenticated);
                let manager = server.reconnection_manager().unwrap();
                drop(original);
                while !manager.has_pending_reconnection(&player_id).await
                    || server.get_client_room(&player_id).await.is_some()
                {
                    tokio::task::yield_now().await;
                }
                let reconnect = |auth_token| ClientMessage::Reconnect {
                    player_id,
                    room_id: room.room_id,
                    auth_token,
                };
                if authenticated {
                    let mut intruder = connect(address).await;
                    authenticate(&mut intruder, &signing, "other-app").await;
                    send(&mut intruder, &reconnect(token.clone())).await;
                    assert!(
                        matches!(
                            receive(&mut intruder).await,
                            ServerMessage::ReconnectionFailed {
                                error_code: ErrorCode::RoomNotFound,
                                ..
                            }
                        ),
                        "another signed application must not restore this player"
                    );
                    assert_eq!(server.get_client_room(&player_id).await, None);
                    assert!(manager.has_pending_reconnection(&player_id).await);
                    drop(intruder);
                }
                let mut recovering = connect_path(cut_address, path).await;
                if authenticated {
                    // Consume both handshake responses before arming the cut.
                    authenticate(&mut recovering, &signing, "owner-app").await;
                }
                if let Some(prefix) = prefix {
                    cut.remaining.store(prefix, Ordering::Release);
                    cut.armed.store(true, Ordering::Release);
                }
                send(&mut recovering, &reconnect(token.clone())).await;
                let rotated = if let Some(prefix) = prefix {
                    let mut bytes = vec![0; prefix];
                    recovering
                        .get_mut()
                        .read_exact(&mut bytes)
                        .await
                        .expect("read frame prefix");
                    cut.parked.notified().await;
                    assert_eq!(bytes[0], 0x81, "prefix starts a final text frame");
                    assert_eq!(cut.remaining.load(Ordering::Acquire), 0);
                    assert!(
                        cut.offered.load(Ordering::Acquire) > prefix,
                        "the production write offered more than the accepted prefix"
                    );
                    if prefix == 32 {
                        assert_eq!(bytes[1] & 0x80, 0, "server frame is unmasked");
                        let (header_length, payload_length) = match bytes[1] & 0x7f {
                            126 => (4, u16::from_be_bytes([bytes[2], bytes[3]]) as u64),
                            127 => (10, u64::from_be_bytes(bytes[2..10].try_into().unwrap())),
                            length => (2, u64::from(length)),
                        };
                        assert!(
                            payload_length + header_length as u64 > prefix as u64,
                            "no full frame reached the client"
                        );
                        assert!(std::str::from_utf8(&bytes[header_length..])
                            .unwrap()
                            .starts_with("{\"type\":\"Reconnected\""));
                    }
                    let mut trailing = [0; 1];
                    assert!(
                        matches!(recovering.get_mut().try_read(&mut trailing),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                        "the parked transport exposes only the permitted prefix"
                    );
                    None
                } else {
                    let ServerMessage::Reconnected(payload) = receive(&mut recovering).await else {
                        panic!("positive control receives baseline");
                    };
                    if path == "ws" {
                        assert!(
                            payload.reconnection_token.is_none(),
                            "v2 keeps its frozen token surface"
                        );
                        None
                    } else {
                        let rotated = payload.reconnection_token.expect("v3 rotated credential");
                        assert_ne!(rotated, token);
                        Some(rotated)
                    }
                };
                assert_eq!(server.client_app_id(&player_id), owner_app);
                drop(recovering);
                while !manager.has_pending_reconnection(&player_id).await
                    || server.get_client_room(&player_id).await.is_some()
                {
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    server.get_client_room(&player_id).await,
                    None,
                    "the closed socket loses its routing assignment"
                );
                let mut retry = connect(address).await;
                if authenticated {
                    authenticate(&mut retry, &signing, "owner-app").await;
                }
                send(&mut retry, &reconnect(token)).await;
                assert!(
                    matches!(
                        receive(&mut retry).await,
                        ServerMessage::ReconnectionFailed {
                            error_code: ErrorCode::ReconnectionTokenInvalid,
                            ..
                        }
                    ),
                    "committed baseline spends the old token even at cut {prefix:?}"
                );
                if let Some(rotated) = rotated {
                    send(&mut retry, &reconnect(rotated)).await;
                    assert!(
                        matches!(receive(&mut retry).await, ServerMessage::Reconnected(payload)
                        if payload.player_id == player_id)
                    );
                    assert_eq!(server.client_app_id(&player_id), owner_app);
                } else {
                    send(
                        &mut retry,
                        &join(Some(room.room_code.clone()), "recovering"),
                    )
                    .await;
                    let ServerMessage::RoomJoined(fresh) = receive(&mut retry).await else {
                        panic!("fresh join recovers after partial write");
                    };
                    assert_eq!(fresh.room_id, room.room_id);
                    assert!(fresh.reconnection_token.is_some());
                    assert_ne!(fresh.player_id, player_id);
                    assert_eq!(server.client_app_id(&fresh.player_id), owner_app);
                    assert_eq!(
                        server.get_client_room(&fresh.player_id).await,
                        Some(room.room_id)
                    );
                    let members = server
                        .database()
                        .get_room_players(&room.room_id)
                        .await
                        .unwrap();
                    assert_eq!(members.len(), 2);
                    assert!(!members.iter().any(|member| member.id == player_id));
                    assert!(members
                        .iter()
                        .any(|member| member.id == fresh.player_id && member.name == "recovering"));
                }
            };
            let result = tokio::time::timeout(
                Duration::from_secs(20),
                std::panic::AssertUnwindSafe(history).catch_unwind(),
            )
            .await;
            server.begin_shutdown_drain();
            server.close_connections_for_shutdown();
            let remaining = server
                .wait_for_shutdown_connections(
                    crate::websocket::registered_connection_shutdown_settle_timeout(),
                )
                .await;
            normal_task.abort();
            cut_task.abort();
            let _ = normal_task.await;
            let _ = cut_task.await;
            assert_eq!(remaining, 0, "partial-write history leaked socket handlers");
            match result {
                Ok(Ok(())) => {}
                Ok(Err(panic)) => std::panic::resume_unwind(panic),
                Err(_) => panic!("partial-write history deadline"),
            }
        }
    }
}
