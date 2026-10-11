// Controlled plaintext AsyncWrite prefixes above a real TLS handshake. These
// histories do not establish TLS-record or kernel-specific write boundaries.
#[cfg(test)]
mod tls_write_history {
    use super::*;
    use axum_server::accept::Accept;
    use hmac::{Hmac, KeyInit, Mac};
    use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName};
    use sha2::{Digest, Sha256};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    type Socket = WebSocketStream<tokio_rustls::client::TlsStream<TcpStream>>;

    #[derive(Clone)]
    struct TlsCutAcceptor<A> {
        inner: A,
        cut: Arc<WriteCut>,
    }

    impl<A, S> Accept<TcpStream, S> for TlsCutAcceptor<A>
    where
        A: Accept<TcpStream, S> + Clone + Send + Sync + 'static,
        A::Future: Send + 'static,
        A::Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        A::Service: Send + 'static,
        S: Send + 'static,
    {
        type Stream = CutStream<A::Stream>;
        type Service = A::Service;
        type Future = futures_util::future::BoxFuture<
            'static,
            std::io::Result<(Self::Stream, Self::Service)>,
        >;

        fn accept(&self, socket: TcpStream, service: S) -> Self::Future {
            let inner = self.inner.clone();
            let cut = Arc::clone(&self.cut);
            Box::pin(async move {
                let (socket, service) = inner.accept(socket, service).await?;
                Ok((CutStream { socket, cut }, service))
            })
        }
    }

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/tls/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    struct BoundSocket {
        socket: Socket,
        secret: Arc<[u8]>,
        fingerprint: String,
        sequence: u64,
    }

    impl BoundSocket {
        async fn connect(address: SocketAddr, path: &str, identity: u16) -> Self {
            let certificate =
                CertificateDer::from_pem_file(fixture(&format!("client-{identity}-cert.pem")))
                    .unwrap();
            let fingerprint = Sha256::digest(certificate.as_ref())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let key = PrivateKeyDer::from_pem_file(fixture(&format!("client-{identity}-key.pem")))
                .unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots
                .add(CertificateDer::from_pem_file(fixture("cert.pem")).unwrap())
                .unwrap();
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_client_auth_cert(vec![certificate], key)
            .unwrap();
            let tcp = TcpStream::connect(address).await.unwrap();
            let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(ServerName::try_from("localhost").unwrap(), tcp)
                .await
                .unwrap();
            let mut request = format!("wss://localhost:{}/{path}", address.port())
                .into_client_request()
                .unwrap();
            request.headers_mut().insert(
                axum::http::header::SEC_WEBSOCKET_PROTOCOL,
                axum::http::HeaderValue::from_static("signalfish.tokenbinding.v2"),
            );
            let handshake_key = request.headers()[axum::http::header::SEC_WEBSOCKET_KEY]
                .to_str()
                .unwrap()
                .to_owned();
            let (mut socket, _) = tokio_tungstenite::client_async(request, tls).await.unwrap();
            let frame = socket.next().await.unwrap().unwrap();
            let challenge: serde_json::Value =
                serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(challenge["type"], "TokenBindingChallenge");
            let nonce = base64::engine::general_purpose::STANDARD
                .decode(challenge["data"]["nonce"].as_str().unwrap())
                .unwrap();
            let secret =
                crate::security::derive_server_nonce_secret(&handshake_key, &nonce).unwrap();
            let mut bound = Self {
                socket,
                secret,
                fingerprint,
                sequence: challenge["data"]["first_sequence"].as_u64().unwrap(),
            };
            bound
                .send(&ClientMessage::Authenticate {
                    app_id: "tls-history".into(),
                    connect_token: None,
                    sdk_version: None,
                    platform: None,
                    game_data_format: None,
                    protocol_version: None,
                    supported_transports: None,
                    supported_topologies: None,
                    requested_capabilities: None,
                })
                .await;
            assert!(matches!(
                receive(&mut bound.socket).await,
                ServerMessage::Authenticated { .. }
            ));
            assert!(matches!(
                receive(&mut bound.socket).await,
                ServerMessage::ProtocolInfo(_)
            ));
            bound
        }

        async fn send(&mut self, message: &ClientMessage) {
            let mut value = serde_json::to_value(message).unwrap();
            let canonical = serde_json::to_vec(&value).unwrap();
            let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.secret).unwrap();
            mac.update(crate::security::TOKEN_BINDING_JSON_DOMAIN);
            mac.update(&self.sequence.to_be_bytes());
            mac.update(&canonical);
            mac.update(self.fingerprint.as_bytes());
            value.as_object_mut().unwrap().insert("token_binding".into(), serde_json::json!({
                "version": crate::security::TOKEN_BINDING_VERSION,
                "scheme": "server_nonce_hkdf_sha256", "sequence": self.sequence,
                "signature": base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()),
                "fingerprint": self.fingerprint,
            }));
            self.sequence += 1;
            self.socket
                .send(TungsteniteMessage::Text(value.to_string().into()))
                .await
                .unwrap();
        }
    }

    async fn wait_disconnected(server: &Arc<EnhancedGameServer>, player: &PlayerId) {
        let manager = server.reconnection_manager().unwrap();
        while !manager.has_pending_reconnection(player).await
            || server.get_client_room(player).await.is_some()
        {
            tokio::task::yield_now().await;
        }
    }

    async fn reject(socket: &mut BoundSocket, message: &ClientMessage) {
        socket.send(message).await;
        assert!(matches!(
            receive(&mut socket.socket).await,
            ServerMessage::ReconnectionFailed {
                error_code: ErrorCode::ReconnectionTokenInvalid,
                ..
            }
        ));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_mtls_partial_reconnect_preserves_certificate_and_gameplay_recovery() {
        for (path, batching, prefix) in ["ws", "v3/ws"].into_iter().flat_map(|path| {
            [false, true].into_iter().flat_map(move |batching| {
                [None, Some(1_usize), Some(32)].map(|prefix| (path, batching, prefix))
            })
        }) {
            let mut config = ServerConfig::default();
            config.websocket_config.enable_batching = batching;
            let transport = crate::config::TransportSecurityConfig {
                tls: crate::config::TlsServerConfig {
                    enabled: true,
                    certificate_path: Some(fixture("server-cert.pem")),
                    private_key_path: Some(fixture("server-key.pem")),
                    client_ca_cert_path: Some(fixture("cert.pem")),
                    client_auth: crate::config::ClientAuthMode::Require,
                },
                token_binding: crate::config::TokenBindingConfig {
                    enabled: true,
                    required: true,
                    require_client_fingerprint: true,
                    ..Default::default()
                },
            };
            let tls = crate::security::build_rustls_config(&transport.tls).unwrap();
            let server = EnhancedGameServer::new(
                config,
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
                DatabaseConfig::InMemory,
                Default::default(),
                Default::default(),
                transport,
                vec![],
            )
            .await
            .unwrap();
            let router = super::super::super::super::routes::create_standalone_router(
                "http://localhost:3000",
            )
            .with_state(Arc::clone(&server));
            let normal = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = normal.local_addr().unwrap();
            let normal_server =
                axum_server::from_tcp_rustls(normal.into_std().unwrap(), tls.clone())
                    .unwrap()
                    .map(crate::security::VerifiedClientCertificateAcceptor::new);
            let normal_task = tokio::spawn(
                normal_server.serve(
                    router
                        .clone()
                        .into_make_service_with_connect_info::<SocketAddr>(),
                ),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let cut_address = listener.local_addr().unwrap();
            let cut = Arc::new(WriteCut::default());
            let cut_server = axum_server::from_tcp_rustls(listener.into_std().unwrap(), tls)
                .unwrap()
                .map(|inner| TlsCutAcceptor {
                    inner: crate::security::VerifiedClientCertificateAcceptor::new(inner),
                    cut: Arc::clone(&cut),
                });
            let cut_task = tokio::spawn(
                cut_server.serve(router.into_make_service_with_connect_info::<SocketAddr>()),
            );
            let history = async {
                let join = |code, name: &str| ClientMessage::JoinRoom {
                    game_name: "tls-write-history".into(),
                    room_code: code,
                    player_name: name.into(),
                    max_players: Some(2),
                    supports_authority: Some(true),
                    relay_transport: None,
                    password: None,
                    join_only: None,
                };
                let mut incumbent = BoundSocket::connect(address, "v3/ws", 101).await;
                incumbent.send(&join(None, "incumbent")).await;
                let ServerMessage::RoomJoined(room) = receive(&mut incumbent.socket).await else {
                    panic!("incumbent baseline")
                };
                let mut original = BoundSocket::connect(address, "v3/ws", 101).await;
                original
                    .send(&join(Some(room.room_code.clone()), "recovering"))
                    .await;
                let ServerMessage::RoomJoined(joined) = receive(&mut original.socket).await else {
                    panic!("original baseline")
                };
                let old = joined.reconnection_token.unwrap();
                let reconnect = |player_id, auth_token| ClientMessage::Reconnect {
                    player_id,
                    room_id: room.room_id,
                    auth_token,
                };
                drop(original);
                wait_disconnected(&server, &joined.player_id).await;
                let mut attacker = BoundSocket::connect(address, "v3/ws", 102).await;
                reject(&mut attacker, &reconnect(joined.player_id, old.clone())).await;
                drop(attacker);
                let mut recovering = BoundSocket::connect(cut_address, path, 101).await;
                if let Some(prefix) = prefix {
                    cut.remaining.store(prefix, Ordering::Release);
                    cut.armed.store(true, Ordering::Release);
                }
                recovering
                    .send(&reconnect(joined.player_id, old.clone()))
                    .await;
                let rotated = if let Some(prefix) = prefix {
                    let mut bytes = vec![0; prefix];
                    recovering
                        .socket
                        .get_mut()
                        .read_exact(&mut bytes)
                        .await
                        .unwrap();
                    cut.parked.notified().await;
                    assert_eq!(bytes[0], 0x81);
                    assert_eq!(cut.remaining.load(Ordering::Acquire), 0);
                    assert!(cut.offered.load(Ordering::Acquire) > prefix);
                    if prefix == 32 {
                        let header = match bytes[1] & 0x7f {
                            126 => 4,
                            127 => 10,
                            _ => 2,
                        };
                        assert!(std::str::from_utf8(&bytes[header..])
                            .unwrap()
                            .starts_with("{\"type\":\"Reconnected\""));
                    }
                    let mut trailing = [0];
                    assert!(
                        tokio::time::timeout(
                            Duration::from_millis(20),
                            recovering.socket.get_mut().read(&mut trailing)
                        )
                        .await
                        .is_err(),
                        "parked TLS transport must expose only its prefix"
                    );
                    None
                } else {
                    let ServerMessage::Reconnected(payload) = receive(&mut recovering.socket).await
                    else {
                        panic!("complete baseline")
                    };
                    assert_eq!(payload.player_id, joined.player_id);
                    assert_eq!(payload.room_id, room.room_id);
                    if path == "ws" {
                        assert!(payload.reconnection_token.is_none());
                        None
                    } else {
                        let token = payload.reconnection_token.unwrap();
                        assert_ne!(token, old);
                        Some(token)
                    }
                };
                // Queue admission is the commit boundary. Finish the old claim
                // before dropping its socket so the next disconnect arms fresh state.
                while server
                    .reconnection_manager()
                    .unwrap()
                    .has_pending_reconnection(&joined.player_id)
                    .await
                {
                    tokio::task::yield_now().await;
                }
                drop(recovering);
                wait_disconnected(&server, &joined.player_id).await;
                let mut retry = BoundSocket::connect(address, "v3/ws", 101).await;
                reject(&mut retry, &reconnect(joined.player_id, old)).await;
                let (mut recovered, restored_id) = if let Some(rotated) = rotated {
                    let mut attacker = BoundSocket::connect(address, "v3/ws", 102).await;
                    reject(&mut attacker, &reconnect(joined.player_id, rotated.clone())).await;
                    drop(attacker);
                    retry.send(&reconnect(joined.player_id, rotated)).await;
                    let ServerMessage::Reconnected(payload) = receive(&mut retry.socket).await
                    else {
                        panic!("rotated recovery")
                    };
                    assert_eq!(payload.player_id, joined.player_id);
                    assert_eq!(payload.room_id, room.room_id);
                    (retry, joined.player_id)
                } else {
                    retry
                        .send(&join(Some(room.room_code.clone()), "recovering"))
                        .await;
                    let ServerMessage::RoomJoined(fresh) = receive(&mut retry.socket).await else {
                        panic!("fresh recovery join")
                    };
                    assert_ne!(fresh.player_id, joined.player_id);
                    assert_eq!(fresh.room_id, room.room_id);
                    let fresh_token = fresh.reconnection_token.unwrap();
                    drop(retry);
                    wait_disconnected(&server, &fresh.player_id).await;
                    let mut attacker = BoundSocket::connect(address, "v3/ws", 102).await;
                    reject(
                        &mut attacker,
                        &reconnect(fresh.player_id, fresh_token.clone()),
                    )
                    .await;
                    drop(attacker);
                    let mut recovered = BoundSocket::connect(address, "v3/ws", 101).await;
                    recovered
                        .send(&reconnect(fresh.player_id, fresh_token))
                        .await;
                    let ServerMessage::Reconnected(payload) = receive(&mut recovered.socket).await
                    else {
                        panic!("fresh credential recovery")
                    };
                    assert_eq!(payload.player_id, fresh.player_id);
                    assert_eq!(payload.room_id, room.room_id);
                    (recovered, fresh.player_id)
                };
                assert_eq!(
                    server.get_client_room(&restored_id).await,
                    Some(room.room_id)
                );
                let members = server
                    .database()
                    .get_room_players(&room.room_id)
                    .await
                    .unwrap();
                assert_eq!(members.len(), 2);
                assert!(members.iter().any(|member| member.id == restored_id));
                assert!(members.iter().any(|member| member.id == room.player_id));
                if restored_id != joined.player_id {
                    assert!(!members.iter().any(|member| member.id == joined.player_id));
                }
                recovered
                    .send(&ClientMessage::GameData {
                        data: serde_json::json!({"recovered": true}),
                        class: None,
                        key: None,
                    })
                    .await;
                loop {
                    if let ServerMessage::GameData {
                        from_player, data, ..
                    } = receive(&mut incumbent.socket).await
                    {
                        assert_eq!(from_player, restored_id);
                        assert_eq!(data, serde_json::json!({"recovered": true}));
                        break;
                    }
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
            assert_eq!(remaining, 0, "TLS history leaked handlers");
            match result {
                Ok(Ok(())) => {}
                Ok(Err(panic)) => std::panic::resume_unwind(panic),
                Err(_) => {
                    panic!("TLS history deadline: {path} batching={batching} prefix={prefix:?}")
                }
            }
        }
    }
}
