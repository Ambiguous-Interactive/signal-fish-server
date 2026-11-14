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
