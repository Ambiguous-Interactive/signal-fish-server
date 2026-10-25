use super::*;
use std::collections::BTreeSet;

// Issue #1200: retain DATA and reciprocal ownership across packet-sized reset batches.
#[test]
fn stream_reset_packets_fit_mtu_and_retry_each_batch_before_reuse() -> Result<()> {
    for mtu in [36_u32, 64, 1191, 1200] {
        let mut local = timed_test_association();
        let mut remote = timed_test_association();
        local.peer_last_tsn = remote.my_next_tsn.wrapping_sub(1);
        remote.peer_last_tsn = local.my_next_tsn.wrapping_sub(1);
        local.peer_next_rsn = Some(remote.my_next_rsn);
        remote.peer_next_rsn = Some(local.my_next_rsn);
        local.mtu = mtu;
        remote.mtu = mtu;
        let stream_count = u16::try_from((mtu - 32) / 2 + 5).unwrap();
        let now = Instant::now();
        for id in 0..stream_count {
            local.open_stream(id, PayloadProtocolIdentifier::Binary)?;
            remote.open_stream(id, PayloadProtocolIdentifier::Binary)?;
            local.stream(id)?.write(now, &id.to_be_bytes())?;
            local.stream(id)?.close(now)?;
        }
        let mut requests = Vec::new();
        let mut old_deliveries = BTreeSet::new();
        for step in 0..200 {
            let at = now + Duration::from_millis(step * 100);
            local.handle_timeout(at);
            remote.handle_timeout(at);
            for raw in local.gather_outbound(at).0 {
                assert!(
                    raw.len() <= mtu as usize,
                    "mtu={mtu}: reset packet has {} bytes",
                    raw.len()
                );
                let packet = Packet::unmarshal(&raw)?;
                if packet
                    .chunks
                    .iter()
                    .any(|chunk| chunk.as_any().is::<ChunkReconfig>())
                {
                    requests.push(raw.clone());
                    if requests.len() == 1 {
                        continue;
                    }
                }
                remote.handle_inbound(packet, at)?;
            }
            for id in 0..stream_count {
                if let Ok(mut stream) = remote.stream(id)
                    && let Some(chunks) = stream.read_sctp()?
                {
                    assert_eq!(
                        chunks.to_payload(2)?.freeze(),
                        Bytes::copy_from_slice(&id.to_be_bytes())
                    );
                    assert!(old_deliveries.insert(id), "old gameplay delivered twice");
                }
            }
            for raw in remote.gather_outbound(at).0 {
                assert!(raw.len() <= mtu as usize);
                local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
            if local.streams.is_empty() && remote.streams.is_empty() {
                break;
            }
        }
        assert!(
            requests.len() >= 3,
            "mtu={mtu}: retries and multiple batches required"
        );
        assert_eq!(requests[0], requests[1], "mtu={mtu}: retry changes request");
        assert!(
            local.streams.is_empty(),
            "mtu={mtu}: local reset incomplete"
        );
        assert!(
            remote.streams.is_empty(),
            "mtu={mtu}: remote reset incomplete"
        );
        assert_eq!(old_deliveries.len(), usize::from(stream_count));
        for id in 0..stream_count {
            local
                .open_stream(id, PayloadProtocolIdentifier::Binary)?
                .write(now, &[0xf1, id.to_be_bytes()[0], id.to_be_bytes()[1]])?;
        }
        for step in 0..10 {
            let at = now + Duration::from_secs(21) + Duration::from_millis(step * 100);
            for raw in local.gather_outbound(at).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
            for raw in remote.gather_outbound(at).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
        }
        for id in 0..stream_count {
            assert_eq!(
                remote
                    .stream(id)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(3)?
                    .freeze(),
                Bytes::copy_from_slice(&[0xf1, id.to_be_bytes()[0], id.to_be_bytes()[1]])
            );
            assert!(
                remote.stream(id)?.read_sctp()?.is_none(),
                "fresh gameplay delivered twice"
            );
        }
    }
    Ok(())
}

#[test]
fn stream_reset_packet_budget_preserves_chunk_length_and_pending_identifiers() -> Result<()> {
    let mut local = timed_test_association();
    local.mtu = 100_000;
    let now = Instant::now();
    for id in 0..33_000 {
        local.queue_reset_request(id);
    }
    let packets = local.gather_outbound(now).0;
    assert_eq!(packets.len(), 1);
    assert!(packets[0].len() <= usize::from(u16::MAX) + 12);
    let packet = Packet::unmarshal(&packets[0])?;
    assert_eq!(packet.chunks.len(), 1);
    let reset = packet.chunks[0]
        .as_any()
        .downcast_ref::<ChunkReconfig>()
        .unwrap();
    let request = reset
        .param_a
        .as_ref()
        .unwrap()
        .as_any()
        .downcast_ref::<ParamOutgoingResetRequest>()
        .unwrap();
    assert!(request.stream_identifiers.len() < 33_000);
    assert_eq!(
        request.stream_identifiers,
        (0..u16::try_from(request.stream_identifiers.len()).unwrap()).collect::<Vec<_>>()
    );
    assert_eq!(
        local.pending_queue.len(),
        33_000 - request.stream_identifiers.len()
    );
    assert!(
        local.gather_outbound(now).0.is_empty(),
        "the next batch must wait for acknowledgment"
    );
    Ok(())
}

// Issue #1201: an impossible reset must leave both stream halves unchanged.
#[test]
fn stream_reset_rejection_preserves_gameplay_and_can_retry() -> Result<()> {
    for payload_budget in [0, 1, 4, 5, 6, 7] {
        for close_both in [false, true] {
            let mut local = timed_test_association();
            let mut remote = timed_test_association();
            local.peer_last_tsn = remote.my_next_tsn.wrapping_sub(1);
            remote.peer_last_tsn = local.my_next_tsn.wrapping_sub(1);
            let now = Instant::now();
            local.open_stream(1, PayloadProtocolIdentifier::Binary)?;
            remote.open_stream(1, PayloadProtocolIdentifier::Binary)?;
            local.stream(1)?.write(now, b"old")?;
            remote.stream(1)?.write(now, b"inbound")?;
            for raw in remote.gather_outbound(now).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            let mut config = EndpointConfig::default();
            config.max_payload_size(payload_budget);
            local.mtu = config.get_max_payload_size() + COMMON_HEADER_SIZE + DATA_CHUNK_HEADER_SIZE;
            let pending = local.pending_queue.len();
            let buffered = local.stream(1)?.buffered_amount()?;
            let events = local.events.len();
            let resets = local.my_next_rsn;
            let result = if close_both {
                local.stream(1)?.close(now)
            } else {
                local.stream(1)?.stop(now)
            };
            assert!(
                matches!(result, Err(Error::ErrOutboundPacketTooLarge)),
                "payload_budget={payload_budget}, close_both={close_both}: {result:?}"
            );
            assert!(local.stream(1)?.is_readable());
            assert!(local.stream(1)?.is_writable());
            assert_eq!(local.pending_queue.len(), pending);
            assert_eq!(local.stream(1)?.buffered_amount()?, buffered);
            assert_eq!(local.events.len(), events);
            assert_eq!(local.my_next_rsn, resets);
            assert!(local.reconfigs.is_empty());
            assert!(!local.streams[&1].reset_requested);
            assert_eq!(local.streams.len(), 1);
            assert_eq!(
                local
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(7)?
                    .freeze(),
                Bytes::from_static(b"inbound")
            );
            local.stream(1)?.write(now, b"tail")?;
            local.mtu = 36;
            local.streams.get_mut(&1).unwrap().max_payload_size = 8;
            local.stream(1)?.close(now)?;
            let mut received = Vec::new();
            for step in 0..100 {
                let at = now + Duration::from_millis(step * 100);
                local.handle_timeout(at);
                remote.handle_timeout(at);
                for raw in local.gather_outbound(at).0 {
                    // DATA was admitted using the earlier default budget; its small values fit.
                    assert!(raw.len() <= 36);
                    remote.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                if let Ok(mut stream) = remote.stream(1) {
                    while let Some(chunks) = stream.read_sctp()? {
                        received.push(chunks.to_payload(4)?.freeze());
                    }
                }
                for raw in remote.gather_outbound(at).0 {
                    local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                if local.streams.is_empty() && remote.streams.is_empty() {
                    break;
                }
            }
            assert_eq!(
                received,
                [Bytes::from_static(b"old"), Bytes::from_static(b"tail")]
            );
            assert!(local.streams.is_empty());
            assert!(remote.streams.is_empty());
        }
    }
    Ok(())
}

#[test]
fn stream_reset_small_budget_denies_remote_reset_without_losing_reader() -> Result<()> {
    for mtu in [28, 29, 32, 35] {
        let mut local = timed_test_association();
        local.mtu = mtu;
        let now = Instant::now();
        local.open_stream(1, PayloadProtocolIdentifier::Binary)?;
        local.stream(1)?.write(now, b"old")?;
        let request = ParamOutgoingResetRequest {
            reconfig_request_sequence_number: local.peer_next_rsn.unwrap_or(10),
            sender_last_tsn: local.peer_last_tsn,
            stream_identifiers: vec![1],
            ..Default::default()
        };
        let request: Box<dyn Param> = Box::new(request);
        for _ in 0..2 {
            let mut reply = Vec::new();
            local.handle_reconfig_param(&request, &mut reply, now)?;
            assert_eq!(reply.len(), 1);
            assert!(reply[0].marshal()?.len() <= mtu as usize);
            assert_eq!(
                local.last_reconfig_reply.as_ref().unwrap().result,
                ReconfigResult::Denied
            );
            assert!(local.reconfig_requests.is_empty());
            assert!(local.reconfigs.is_empty());
            assert!(local.stream(1)?.is_readable());
            assert!(local.stream(1)?.is_writable());
            assert!(!local.streams[&1].incoming_reset_complete);
            assert!(!local.streams[&1].reset_requested);
            assert_eq!(local.pending_queue.len(), 1);
            assert_eq!(local.stream(1)?.buffered_amount()?, 3);
        }
    }
    Ok(())
}
