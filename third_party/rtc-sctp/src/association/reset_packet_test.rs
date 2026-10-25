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
