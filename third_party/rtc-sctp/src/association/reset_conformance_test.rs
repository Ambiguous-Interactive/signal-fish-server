use super::*;

/// #1184: qualify the cross-stream implicit-ack policy used by usrsctp.
/// The remote side is a packet script; strict RFC interpretation stays qualified.
#[test]
fn scripted_peer_preserves_future_sequence_through_graceful_shutdown() -> Result<()> {
    for remote_initial in [100_u32, u32::MAX] {
        let mut receiver = timed_test_association();
        let now = Instant::now();
        receiver.peer_last_tsn = remote_initial.wrapping_sub(1);
        receiver.peer_next_rsn = Some(remote_initial);
        receiver.ack_mode = AckMode::NoDelay;
        receiver.source_port = 5000;
        receiver.destination_port = 5001;
        receiver.my_verification_tag = 0x1020_3040;
        receiver.peer_verification_tag = 0x5060_7080;
        receiver.handshake_completed = true;
        let ppi = PayloadProtocolIdentifier::Binary;
        receiver.open_stream(1, ppi)?;
        receiver.open_stream(2, ppi)?;
        let header = CommonHeader {
            source_port: receiver.destination_port,
            destination_port: receiver.source_port,
            verification_tag: receiver.my_verification_tag,
        };
        let wire_time = std::cell::Cell::new(now);
        let deliver = |receiver: &mut Association, chunk: Box<dyn Chunk>| -> Result<()> {
            let packet = Packet {
                common_header: CommonHeader {
                    source_port: header.source_port,
                    destination_port: header.destination_port,
                    verification_tag: header.verification_tag,
                },
                chunks: vec![chunk],
            };
            receiver.handle_inbound(Packet::unmarshal(&packet.marshal()?)?, wire_time.get())?;
            Ok(())
        };
        let old = Bytes::from_static(b"old unread gameplay");
        deliver(
            &mut receiver,
            Box::new(ChunkPayloadData {
                tsn: remote_initial,
                stream_identifier: 1,
                payload_type: ppi,
                beginning_fragment: true,
                ending_fragment: true,
                user_data: old.clone(),
                ..Default::default()
            }),
        )?;
        receiver.send_reset_request(now, 1)?;
        let requests = receiver.gather_outbound(now).0;
        let first = outgoing_request(&requests);
        assert_eq!(first.stream_identifiers, [1]);
        assert_eq!(
            first.reconfig_response_sequence_number,
            remote_initial.wrapping_sub(1)
        );
        assert_eq!(first.sender_last_tsn, receiver.my_next_tsn.wrapping_sub(1));

        // RFC 6525 5.2.2 E3/E5: the remote incoming stream has no unread DATA.
        deliver(
            &mut receiver,
            Box::new(ChunkReconfig {
                param_a: Some(response(
                    first.reconfig_request_sequence_number,
                    ReconfigResult::SuccessPerformed,
                )),
                param_b: None,
            }),
        )?;
        // RFC 6525 5.1.2 A2-A4: first remote request, final old TSN, last local RSN.
        deliver(
            &mut receiver,
            Box::new(ChunkReconfig {
                param_a: Some(Box::new(ParamOutgoingResetRequest {
                    reconfig_request_sequence_number: remote_initial,
                    reconfig_response_sequence_number: first.reconfig_request_sequence_number,
                    sender_last_tsn: remote_initial,
                    stream_identifiers: vec![1],
                })),
                param_b: None,
            }),
        )?;
        assert_eq!(receiver.peer_next_rsn, Some(remote_initial.wrapping_add(1)));
        let replies = receiver.gather_outbound(now).0;
        assert_eq!(
            reset_result(&replies, remote_initial),
            Some(ReconfigResult::InProgress)
        );
        assert!(receiver.streams[&1].has_readable_data());

        // The local first request is acknowledged, so 5.1.1 permits a new request.
        receiver.send_reset_request(now, 2)?;
        let second = outgoing_request(&receiver.gather_outbound(now).0);
        assert_eq!(second.stream_identifiers, [2]);
        assert_eq!(
            second.reconfig_request_sequence_number,
            first.reconfig_request_sequence_number.wrapping_add(1)
        );
        assert_eq!(second.reconfig_response_sequence_number, remote_initial);
        /* usrsctp treats this E1 acknowledgment as performed for the old SID. */
        deliver(
            &mut receiver,
            Box::new(ChunkReconfig {
                param_a: Some(response(
                    second.reconfig_request_sequence_number,
                    ReconfigResult::SuccessPerformed,
                )),
                param_b: None,
            }),
        )?;
        // The peer assigns future SSN 0, loses that packet, then expires it.
        wire_time.set(now + Duration::from_secs(2));
        deliver(
            &mut receiver,
            Box::new(ChunkForwardTsn {
                new_cumulative_tsn: remote_initial.wrapping_add(1),
                streams: vec![ChunkForwardTsnStream {
                    identifier: 1,
                    sequence: 0,
                }],
            }),
        )?;
        let now = wire_time.get();
        receiver.shutdown()?;
        assert_eq!(receiver.state(), AssociationState::ShutdownSent);
        assert_eq!(
            receiver
                .stream(1)?
                .read_sctp()?
                .unwrap()
                .to_payload(100)?
                .freeze(),
            old
        );
        let next = Bytes::from_static(b"reliable future one");
        // This DATA was sent before the peer received our SHUTDOWN.
        deliver(
            &mut receiver,
            Box::new(ChunkPayloadData {
                tsn: remote_initial.wrapping_add(2),
                stream_identifier: 1,
                stream_sequence_number: 1,
                payload_type: ppi,
                beginning_fragment: true,
                ending_fragment: true,
                user_data: next.clone(),
                ..Default::default()
            }),
        )?;
        assert_eq!(receiver.peer_last_tsn, remote_initial.wrapping_add(2));
        assert!(!receiver.stream(1)?.is_writable());
        assert_eq!(
            receiver.stream(1)?.write_sctp(now, &next, ppi),
            Err(Error::ErrStreamClosed)
        );
        assert!(receiver.gather_outbound(now).0.iter().any(|raw| {
            Packet::unmarshal(raw)
                .unwrap()
                .chunks
                .iter()
                .any(|chunk| chunk.as_any().is::<ChunkShutdown>())
        }));
        deliver(&mut receiver, Box::new(ChunkShutdownAck))?;
        let final_packets = receiver.gather_outbound(now).0;
        assert!(final_packets.iter().any(|raw| {
            Packet::unmarshal(raw)
                .unwrap()
                .chunks
                .iter()
                .any(|chunk| chunk.as_any().is::<ChunkShutdownComplete>())
        }));
        assert_eq!(receiver.state(), AssociationState::Closed);
        assert!(
            receiver
                .streams
                .get(&1)
                .is_some_and(|stream| stream.has_readable_data()),
            "accepted future SSN 1 must be readable through terminal closure"
        );
        assert!(receiver.poll_endpoint_event().is_none());
        assert_eq!(
            receiver
                .stream(1)?
                .read_sctp()?
                .expect("accepted SSN 1 must remain readable after terminal closure")
                .to_payload(100)?
                .freeze(),
            next
        );
        assert!(!receiver.streams.contains_key(&2));
        assert!(receiver.reset_ssn_markers.is_empty());
        assert!(receiver.poll_timeout().is_none());
        assert!(receiver.gather_outbound(now).0.is_empty());
        assert!(receiver.streams.is_empty());
        assert!(receiver.poll_endpoint_event().unwrap().is_drained());
        assert!(receiver.poll_endpoint_event().is_none());
        assert!(receiver.open_stream(1, ppi).is_err());
    }
    Ok(())
}

/// #1184: crossed resets need no implicit success or application-drain extension.
#[test]
fn crossed_successful_resets_preserve_future_sequence_during_shutdown() -> Result<()> {
    let mut sender = timed_test_association();
    let mut receiver = timed_test_association();
    sender.source_port = 5000;
    sender.destination_port = 5001;
    receiver.source_port = 5001;
    receiver.destination_port = 5000;
    sender.my_verification_tag = 0x1020_3040;
    receiver.my_verification_tag = 0x5060_7080;
    sender.peer_verification_tag = receiver.my_verification_tag;
    receiver.peer_verification_tag = sender.my_verification_tag;
    sender.peer_last_tsn = receiver.my_next_tsn.wrapping_sub(1);
    receiver.peer_last_tsn = sender.my_next_tsn.wrapping_sub(1);
    sender.peer_next_rsn = Some(receiver.my_next_rsn);
    receiver.peer_next_rsn = Some(sender.my_next_rsn);
    sender.handshake_completed = true;
    receiver.handshake_completed = true;
    sender.ack_mode = AckMode::NoDelay;
    receiver.ack_mode = AckMode::NoDelay;
    let now = Instant::now();
    let ppi = PayloadProtocolIdentifier::Binary;
    sender.open_stream(1, ppi)?;
    receiver.open_stream(1, ppi)?;

    let old = Bytes::from_static(b"old gameplay read before crossed resets");
    sender.stream(1)?.write_sctp(now, &old, ppi)?;
    deliver_packets(&mut receiver, sender.gather_outbound(now).0, now)?;
    assert_eq!(
        receiver
            .stream(1)?
            .read_sctp()?
            .unwrap()
            .to_payload(100)?
            .freeze(),
        old
    );
    deliver_packets(&mut sender, receiver.gather_outbound(now).0, now)?;
    assert!(sender.inflight_queue.is_empty());

    sender.send_reset_request(now, 1)?;
    receiver.send_reset_request(now, 1)?;
    let sender_request_packets = sender.gather_outbound(now).0;
    let receiver_request_packets = receiver.gather_outbound(now).0;
    let sender_request = outgoing_request(&sender_request_packets);
    let receiver_request = outgoing_request(&receiver_request_packets);
    assert_eq!(sender_request.stream_identifiers, [1]);
    assert_eq!(receiver_request.stream_identifiers, [1]);
    assert_eq!(
        sender_request.reconfig_response_sequence_number,
        receiver_request
            .reconfig_request_sequence_number
            .wrapping_sub(1)
    );
    assert_eq!(
        receiver_request.reconfig_response_sequence_number,
        sender_request
            .reconfig_request_sequence_number
            .wrapping_sub(1)
    );
    assert_eq!(sender_request.sender_last_tsn, receiver.peer_last_tsn);
    assert_eq!(receiver_request.sender_last_tsn, sender.peer_last_tsn);

    deliver_packets(&mut receiver, sender_request_packets, now)?;
    deliver_packets(&mut sender, receiver_request_packets, now)?;
    let sender_success_packets = sender.gather_outbound(now).0;
    let receiver_success_packets = receiver.gather_outbound(now).0;
    for packets in [&sender_success_packets, &receiver_success_packets] {
        assert_eq!(packets.len(), 1);
        let packet = Packet::unmarshal(&packets[0])?;
        assert_eq!(packet.chunks.len(), 1);
        let reconfig = packet.chunks[0]
            .as_any()
            .downcast_ref::<ChunkReconfig>()
            .expect("a genuine success contains only its RE-CONFIG response");
        assert!(reconfig.param_b.is_none());
        let response = reconfig
            .param_a
            .as_ref()
            .unwrap()
            .as_any()
            .downcast_ref::<ParamReconfigResponse>()
            .unwrap();
        assert_eq!(response.result, ReconfigResult::SuccessPerformed);
    }
    assert_eq!(
        reset_result(
            &sender_success_packets,
            receiver_request.reconfig_request_sequence_number
        ),
        Some(ReconfigResult::SuccessPerformed)
    );
    assert_eq!(
        reset_result(
            &receiver_success_packets,
            sender_request.reconfig_request_sequence_number
        ),
        Some(ReconfigResult::SuccessPerformed)
    );
    for association in [&sender, &receiver] {
        let stream = &association.streams[&1];
        assert!(stream.incoming_reset_complete);
        assert!(!stream.outgoing_reset_complete);
        assert!(association.reconfig_requests.is_empty());
        assert_eq!(association.reconfigs.len(), 1);
    }

    /* Delay only the genuine successful response to the receiver's reset. */
    deliver_packets(&mut sender, receiver_success_packets, now)?;
    assert!(!sender.streams.contains_key(&1));
    sender
        .open_stream(1, ppi)?
        .set_reliability_params(false, ReliabilityType::Timed, 1)?;
    sender
        .stream(1)?
        .write_sctp(now, &Bytes::from_static(b"expired future zero"), ppi)?;
    let lost_packets = sender.gather_outbound(now).0;
    let lost = transmitted_data(&lost_packets);
    assert_eq!(lost.len(), 1);
    assert_eq!(lost[0].stream_sequence_number, 0);
    let later = now + Duration::from_secs(2);
    sender.handle_timeout(later);
    let forwarded_packets = sender.gather_outbound(later).0;
    let forward: Vec<_> = forwarded_packets
        .iter()
        .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
        .filter_map(|chunk| chunk.as_any().downcast_ref::<ChunkForwardTsn>().cloned())
        .collect();
    assert_eq!(forward.len(), 1);
    assert_eq!(forward[0].new_cumulative_tsn, lost[0].tsn);
    assert_eq!(forward[0].streams.len(), 1);
    assert_eq!(forward[0].streams[0].identifier, 1);
    assert_eq!(forward[0].streams[0].sequence, 0);
    deliver_packets(&mut receiver, forwarded_packets, later)?;
    assert_eq!(receiver.streams[&1].deferred_reset_next_ssn, Some(1));
    assert!(receiver.streams[&1].incoming_reset_complete);
    assert!(!receiver.streams[&1].outgoing_reset_complete);

    receiver.shutdown()?;
    assert_eq!(receiver.state(), AssociationState::ShutdownSent);
    assert_eq!(sender.state(), AssociationState::Established);
    deliver_packets(&mut receiver, sender_success_packets, later)?;
    assert!(!receiver.streams.contains_key(&1));

    let next = Bytes::from_static(b"reliable one after crossed reset and shutdown");
    /* The sender has not received SHUTDOWN; its new DATA crosses that packet. */
    sender
        .stream(1)?
        .set_reliability_params(false, ReliabilityType::Reliable, 0)?;
    sender.stream(1)?.write_sctp(later, &next, ppi)?;
    let successor_packets = sender.gather_outbound(later).0;
    let successor = transmitted_data(&successor_packets);
    assert_eq!(successor.len(), 1);
    assert_eq!(successor[0].stream_sequence_number, 1);
    deliver_packets(&mut receiver, successor_packets, later)?;
    assert_eq!(receiver.peer_last_tsn, successor[0].tsn);
    assert!(
        receiver.streams[&1].has_readable_data(),
        "accepted future SSN 1 must remain readable after genuine crossed reset success during ShutdownSent"
    );

    for _ in 0..8 {
        deliver_packets(&mut sender, receiver.gather_outbound(later).0, later)?;
        deliver_packets(&mut receiver, sender.gather_outbound(later).0, later)?;
    }
    assert_eq!(receiver.state(), AssociationState::Closed);
    assert_eq!(sender.state(), AssociationState::Closed);
    assert!(receiver.poll_endpoint_event().is_none());
    assert!(!receiver.stream(1)?.is_writable());
    assert_eq!(
        receiver.stream(1)?.write_sctp(later, &next, ppi),
        Err(Error::ErrStreamClosed)
    );
    assert_eq!(
        receiver
            .stream(1)?
            .read_sctp()?
            .unwrap()
            .to_payload(100)?
            .freeze(),
        next
    );
    for endpoint in [&mut sender, &mut receiver] {
        assert!(endpoint.streams.is_empty());
        assert!(endpoint.reset_ssn_markers.is_empty());
        assert!(endpoint.poll_timeout().is_none());
        assert!(endpoint.gather_outbound(later).0.is_empty());
        assert!(endpoint.poll_endpoint_event().unwrap().is_drained());
        assert!(endpoint.poll_endpoint_event().is_none());
        assert!(endpoint.open_stream(1, ppi).is_err());
    }
    println!(
        "CROSSED_RESET shutdown=ShutdownSent receiver_success=SuccessPerformed sender_success=SuccessPerformed old_read=exact expired_ssn=0 forwarded_ssn=0 accepted_ssn=1 terminal_read=exact endpoint_drains=2"
    );
    Ok(())
}

fn deliver_packets(receiver: &mut Association, packets: Vec<Bytes>, now: Instant) -> Result<()> {
    for raw in packets {
        receiver.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    Ok(())
}

fn outgoing_request(raws: &[Bytes]) -> ParamOutgoingResetRequest {
    let requests: Vec<_> = raws
        .iter()
        .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
        .filter_map(|chunk| chunk.as_any().downcast_ref::<ChunkReconfig>().cloned())
        .flat_map(|chunk| [chunk.param_a, chunk.param_b].into_iter().flatten())
        .filter_map(|param| {
            param
                .as_any()
                .downcast_ref::<ParamOutgoingResetRequest>()
                .cloned()
        })
        .collect();
    assert_eq!(requests.len(), 1);
    requests.into_iter().next().unwrap()
}

fn reset_result(raws: &[Bytes], sequence: u32) -> Option<ReconfigResult> {
    raws.iter()
        .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
        .filter_map(|chunk| chunk.as_any().downcast_ref::<ChunkReconfig>().cloned())
        .flat_map(|chunk| [chunk.param_a, chunk.param_b].into_iter().flatten())
        .filter_map(|param| {
            param
                .as_any()
                .downcast_ref::<ParamReconfigResponse>()
                .cloned()
        })
        .find(|response| response.reconfig_response_sequence_number == sequence)
        .map(|response| response.result)
}

fn response(sequence: u32, result: ReconfigResult) -> Box<dyn Param> {
    Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: sequence,
        result,
    })
}
