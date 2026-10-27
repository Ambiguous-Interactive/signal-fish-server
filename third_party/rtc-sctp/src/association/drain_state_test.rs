use super::*;

fn established_pair() -> (Association, Association) {
    let make = || {
        let mut association = Association::new(
            None,
            Arc::new(TransportConfig::default()),
            1400,
            0,
            SocketAddr::from(([127, 0, 0, 1], 0)),
            SocketAddr::from(([127, 0, 0, 1], 0)),
            TransportProtocol::UDP,
            Instant::now(),
        );
        association.control_queue.clear();
        association.timers.stop(Timer::T1Init);
        association.set_state(AssociationState::Established);
        association.rwnd = 65536;
        association.ack_mode = AckMode::NoDelay;
        association
            .open_stream(1, PayloadProtocolIdentifier::Binary)
            .unwrap();
        association
    };
    let mut local = make();
    let mut remote = make();
    local.peer_last_tsn = remote.my_next_tsn.wrapping_sub(1);
    remote.peer_last_tsn = local.my_next_tsn.wrapping_sub(1);
    (local, remote)
}

#[test]
fn drain_readiness_keeps_admitted_gameplay_during_shutdown() -> Result<()> {
    let (mut local, mut remote) = established_pair();
    let now = Instant::now();
    let expected = Bytes::from_static(b"admitted gameplay survives public drain checks");
    local
        .stream(1)?
        .write_sctp(now, &expected, PayloadProtocolIdentifier::Binary)?;
    local.shutdown()?;
    assert_eq!(local.state(), AssociationState::ShutdownPending);
    assert!(
        !local.is_drained(),
        "unsent admitted DATA still needs its driver"
    );
    let mut received = Vec::new();
    for _ in 0..8 {
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            if remote.streams.contains_key(&1) {
                while let Some(message) = remote.stream(1)?.read_sctp()? {
                    received.push(message.into_payload(1024)?.freeze());
                }
            }
        }
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        if local.state() != AssociationState::Closed {
            assert!(
                !local.is_drained(),
                "the shutdown handshake still needs its driver"
            );
        }
    }
    assert_eq!(received, vec![expected]);
    for association in [&mut local, &mut remote] {
        assert_eq!(association.state(), AssociationState::Closed);
        assert!(association.is_drained());
        assert!(association.poll_timeout().is_none());
        assert!(association.poll_endpoint_event().unwrap().is_drained());
        assert!(association.poll_endpoint_event().is_none());
        assert!(
            association.is_drained(),
            "consuming the event does not revive work"
        );
    }
    Ok(())
}

#[test]
fn drain_readiness_retains_closed_unread_gameplay_until_read_or_stop() -> Result<()> {
    for stop in [false, true] {
        let (mut local, mut remote) = established_pair();
        let now = Instant::now();
        let expected = Bytes::from_static(b"accepted unread gameplay owns the association");
        remote
            .stream(1)?
            .write_sctp(now, &expected, PayloadProtocolIdentifier::Binary)?;
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        local.shutdown()?;
        for _ in 0..8 {
            for raw in local.gather_outbound(now).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            for raw in remote.gather_outbound(now).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
        }
        assert_eq!(local.state(), AssociationState::Closed);
        assert!(
            !local.is_drained(),
            "stop={stop}: unread accepted DATA still belongs to its reader"
        );
        assert!(local.poll_endpoint_event().is_none());
        if stop {
            local.stream(1)?.stop(now)?;
        } else {
            assert_eq!(
                local
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .into_payload(1024)?
                    .freeze(),
                expected
            );
        }
        assert!(
            local.is_drained(),
            "stop={stop}: final application ownership is released"
        );
        assert!(local.poll_endpoint_event().unwrap().is_drained());
        assert!(local.poll_endpoint_event().is_none());
    }
    Ok(())
}

#[test]
fn drain_readiness_waits_for_abort_packet_handoff() -> Result<()> {
    let (mut local, _) = established_pair();
    let now = Instant::now();
    local.abort()?;
    assert!(local.is_closed());
    assert!(
        !local.is_drained(),
        "the final ABORT packet is still owned by the association"
    );
    let transmit = local
        .poll_transmit(now)
        .expect("handoff the final ABORT packet");
    let Payload::RawEncode(packets) = transmit.message else {
        panic!("SCTP emits encoded packets");
    };
    assert_eq!(packets.len(), 1);
    let packet = Packet::unmarshal(&packets[0])?;
    assert_eq!(packet.chunks.len(), 1);
    assert!(packet.chunks[0].as_any().is::<ChunkAbort>());
    assert!(local.is_drained());
    assert!(local.poll_timeout().is_none());
    Ok(())
}

#[test]
fn drain_readiness_does_not_retire_fresh_or_live_associations() {
    let mut association = Association::default();
    assert!(association.is_closed());
    assert!(
        !association.is_drained(),
        "an unstarted association is not retired"
    );
    for state in [
        AssociationState::CookieWait,
        AssociationState::CookieEchoed,
        AssociationState::Established,
        AssociationState::ShutdownPending,
        AssociationState::ShutdownReceived,
        AssociationState::ShutdownSent,
        AssociationState::ShutdownAckSent,
    ] {
        association.set_state(state);
        assert!(
            !association.is_drained(),
            "{state:?} still needs its driver"
        );
    }
}
