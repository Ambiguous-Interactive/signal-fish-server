use super::*;
use crate::TimerConfig;

fn reset_pair() -> (Association, Association) {
    let mut a = timed_test_association();
    let mut b = timed_test_association();
    a.peer_last_tsn = b.my_next_tsn.wrapping_sub(1);
    b.peer_last_tsn = a.my_next_tsn.wrapping_sub(1);
    a.peer_next_rsn = Some(b.my_next_rsn);
    b.peer_next_rsn = Some(a.my_next_rsn);
    // Establish the RFC 6525 initial request sequences without a handshake.
    a.open_stream(1, PayloadProtocolIdentifier::Binary).unwrap();
    b.open_stream(1, PayloadProtocolIdentifier::Binary).unwrap();
    (a, b)
}

fn reset_requests(a: &mut Association, now: Instant) -> Vec<ChunkReconfig> {
    a.gather_outbound(now)
        .0
        .iter()
        .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
        .filter_map(|c| c.as_any().downcast_ref::<ChunkReconfig>().cloned())
        .collect()
}

fn response(rsn: u32, result: ReconfigResult) -> Box<dyn Param> {
    Box::new(ParamReconfigResponse {
        reconfig_response_sequence_number: rsn,
        result,
    })
}

fn request(c: &ChunkReconfig) -> ParamOutgoingResetRequest {
    c.param_a
        .as_ref()
        .unwrap()
        .as_any()
        .downcast_ref::<ParamOutgoingResetRequest>()
        .unwrap()
        .clone()
}

// Issue #872: neither reset direction alone makes the SID reusable.
#[test]
fn stream_reset_waits_for_both_directions_then_reuses_sid() -> Result<()> {
    let (mut a, mut b) = reset_pair();
    let now = Instant::now();
    a.send_reset_request(now, 1)?;
    let outgoing = reset_requests(&mut a, now).remove(0);
    let replies = b.handle_reconfig(&outgoing, now)?;
    assert!(
        b.streams.contains_key(&1),
        "incoming reset must reserve SID until reciprocal acknowledgment"
    );
    assert!(
        !b.events
            .iter()
            .any(|e| matches!(e, Event::AssociationLost { .. }))
    );
    for packet in replies {
        for c in packet.chunks {
            if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                a.handle_reconfig(c, now)?;
            }
        }
    }
    assert!(
        a.streams.contains_key(&1),
        "outgoing acknowledgment alone must reserve SID"
    );
    let reciprocal = reset_requests(&mut b, now).remove(0);
    let replies = a.handle_reconfig(&reciprocal, now)?;
    assert!(!a.streams.contains_key(&1));
    for packet in replies {
        for c in packet.chunks {
            if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                b.handle_reconfig(c, now)?;
            }
        }
    }
    assert!(!b.streams.contains_key(&1));
    a.open_stream(1, PayloadProtocolIdentifier::Binary)?
        .write_sctp(
            now,
            &Bytes::from_static(b"new generation"),
            PayloadProtocolIdentifier::Binary,
        )?;
    b.open_stream(1, PayloadProtocolIdentifier::Binary)?;
    for d in transmitted_data(&a.gather_outbound(now).0) {
        b.handle_data(&d)?;
    }
    assert_eq!(
        b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
        Bytes::from_static(b"new generation")
    );
    Ok(())
}

#[test]
fn stream_reset_in_progress_retains_request_and_retransmits() -> Result<()> {
    let (mut a, _) = reset_pair();
    let now = Instant::now();
    a.send_reset_request(now, 1)?;
    let original = reset_requests(&mut a, now).remove(0);
    let rsn = request(&original).reconfig_request_sequence_number;
    a.handle_reconfig_param(&response(rsn, ReconfigResult::InProgress), &mut vec![], now)?;
    assert!(
        a.reconfigs.contains_key(&rsn),
        "InProgress must retain the outgoing request"
    );
    for _ in 0..8 {
        let at = a
            .poll_timeout()
            .expect("InProgress keeps the retry timer armed");
        a.handle_timeout(at);
        let retransmit = reset_requests(&mut a, at);
        assert_eq!(retransmit.len(), 1);
        assert_eq!(request(&retransmit[0]), request(&original));
        assert_eq!(
            a.state(),
            AssociationState::Established,
            "InProgress retry must not exhaust error budget"
        );
    }
    Ok(())
}

#[test]
fn stream_reset_duplicate_cannot_remove_reopened_sid() -> Result<()> {
    let (mut a, mut b) = reset_pair();
    let now = Instant::now();
    a.send_reset_request(now, 1)?;
    let original = reset_requests(&mut a, now).remove(0);
    let first = b.handle_reconfig(&original, now)?;
    let rsn = b.my_next_rsn;
    b.handle_reconfig(&original, now)?;
    assert_eq!(
        b.my_next_rsn, rsn,
        "duplicate request must not create another reciprocal reset"
    );
    let reciprocal = first
        .iter()
        .flat_map(|p| &p.chunks)
        .filter_map(|c| c.as_any().downcast_ref::<ChunkReconfig>())
        .find(|c| {
            c.param_a
                .as_ref()
                .is_some_and(|p| p.as_any().is::<ParamOutgoingResetRequest>())
        })
        .cloned()
        .unwrap_or_else(|| reset_requests(&mut b, now).remove(0));
    b.handle_reconfig_param(
        &response(
            request(&reciprocal).reconfig_request_sequence_number,
            ReconfigResult::SuccessPerformed,
        ),
        &mut vec![],
        now,
    )?;
    b.open_stream(1, PayloadProtocolIdentifier::Binary)?;
    let replay = b.handle_reconfig(&original, now)?;
    assert!(
        b.streams.contains_key(&1),
        "replay must not reset the new stream generation"
    );
    assert_eq!(
        first
            .iter()
            .map(|p| p.marshal().unwrap())
            .collect::<Vec<_>>(),
        replay
            .iter()
            .map(|p| p.marshal().unwrap())
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn stream_reset_new_data_before_final_ack_survives() -> Result<()> {
    for unordered in [false, true] {
        let (mut a, mut b) = reset_pair();
        let now = Instant::now();
        let ppi = PayloadProtocolIdentifier::Binary;
        a.send_reset_request(now, 1)?;
        let outgoing = reset_requests(&mut a, now).remove(0);
        for p in b.handle_reconfig(&outgoing, now)? {
            for c in p.chunks {
                if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                    a.handle_reconfig(c, now)?;
                }
            }
        }
        let reciprocal = reset_requests(&mut b, now).remove(0);
        let final_ack = a.handle_reconfig(&reciprocal, now)?;
        assert!(!a.streams.contains_key(&1));
        a.open_stream(1, ppi)?
            .set_reliability_params(unordered, ReliabilityType::Rexmit, 0)?;
        a.stream(1)?
            .write_sctp(now, &Bytes::from_static(b"first new generation DATA"), ppi)?;
        let new_data = transmitted_data(&a.gather_outbound(now).0).remove(0);
        b.handle_data(&new_data)?;
        b.handle_data(&new_data)?; // duplicate cannot consume the window twice
        assert_eq!(b.deferred_reset_data.len(), 1);
        assert_eq!(
            b.create_selective_ack_chunk().cumulative_tsn_ack,
            new_data.tsn,
            "accepted held DATA must be acknowledged before reset completion"
        );
        let sack = b
            .gather_outbound(now)
            .0
            .iter()
            .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
            .find_map(|c| {
                c.as_any()
                    .downcast_ref::<ChunkSelectiveAck>()
                    .map(|s| ChunkSelectiveAck {
                        cumulative_tsn_ack: s.cumulative_tsn_ack,
                        advertised_receiver_window_credit: s.advertised_receiver_window_credit,
                        gap_ack_blocks: s.gap_ack_blocks.clone(),
                        duplicate_tsn: s.duplicate_tsn.clone(),
                    })
            })
            .expect("held DATA must emit an actual SACK before the final reset ACK");
        a.handle_sack(&sack, now + Duration::from_millis(1))?;
        assert!(
            a.inflight_queue.is_empty(),
            "held payload must not exhaust its zero-retry budget"
        );
        a.handle_timeout(now + Duration::from_secs(2));
        assert!(transmitted_data(&a.gather_outbound(now + Duration::from_secs(2)).0).is_empty());
        assert!(!b.streams.get(&1).unwrap().reassembly_queue.is_readable());
        for p in final_ack {
            for c in p.chunks {
                if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                    b.handle_reconfig(c, now)?;
                }
            }
        }
        assert!(b.deferred_reset_data.is_empty());
        let reopened_credit = b
            .gather_outbound(now)
            .0
            .iter()
            .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
            .find_map(|c| {
                c.as_any()
                    .downcast_ref::<ChunkSelectiveAck>()
                    .map(|s| ChunkSelectiveAck {
                        cumulative_tsn_ack: s.cumulative_tsn_ack,
                        advertised_receiver_window_credit: s.advertised_receiver_window_credit,
                        gap_ack_blocks: s.gap_ack_blocks.clone(),
                        duplicate_tsn: s.duplicate_tsn.clone(),
                    })
            })
            .expect("released metadata must advertise its restored receive window");
        assert!(
            reopened_credit.advertised_receiver_window_credit
                > sack.advertised_receiver_window_credit
        );
        assert_eq!(
            b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"first new generation DATA")
        );
        assert_eq!(b.peer_last_tsn, new_data.tsn);
        assert!(b.stream(1)?.read_sctp()?.is_none());
        b.handle_data(&new_data)?;
        assert!(
            b.stream(1)?.read_sctp()?.is_none(),
            "late duplicates must not deliver a second copy"
        );
    }
    Ok(())
}

#[test]
fn stream_reset_sequence_wrap_rejects_stale_future_and_changed_replay() -> Result<()> {
    let (_, mut b) = reset_pair();
    let now = Instant::now();
    b.peer_next_rsn = Some(u32::MAX);
    let req = |seq, id| -> Box<dyn Param> {
        Box::new(ParamOutgoingResetRequest {
            reconfig_request_sequence_number: seq,
            reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
            sender_last_tsn: b.peer_last_tsn,
            stream_identifiers: vec![id],
        })
    };
    let first = req(u32::MAX, 1);
    let stale = req(u32::MAX - 1, 1);
    let future = req(1, 1);
    let changed = req(u32::MAX, 2);
    b.open_stream(2, PayloadProtocolIdentifier::Binary)?;
    b.handle_reconfig_param(&first, &mut vec![], now)?;
    assert_eq!(b.peer_next_rsn, Some(0));
    for p in [stale, future] {
        let mut reply = vec![];
        b.handle_reconfig_param(&p, &mut reply, now)?;
        let c = reply[0].chunks[0]
            .as_any()
            .downcast_ref::<ChunkReconfig>()
            .unwrap();
        assert_eq!(
            c.param_a
                .as_ref()
                .unwrap()
                .as_any()
                .downcast_ref::<ParamReconfigResponse>()
                .unwrap()
                .result,
            ReconfigResult::ErrorBadSequenceNumber
        );
        assert_eq!(b.peer_next_rsn, Some(0));
    }
    b.handle_reconfig_param(&changed, &mut vec![], now)?;
    assert!(!b.streams.get(&2).unwrap().incoming_reset_complete);
    b.my_next_rsn = u32::MAX;
    assert_eq!(b.generate_next_rsn(), u32::MAX);
    assert_eq!(b.generate_next_rsn(), 0);
    Ok(())
}

#[test]
fn stream_reset_denied_or_unmatched_response_never_releases_sid() -> Result<()> {
    for result in [
        ReconfigResult::Denied,
        ReconfigResult::ErrorBadSequenceNumber,
        ReconfigResult::Unknown,
    ] {
        let (mut a, _) = reset_pair();
        let now = Instant::now();
        a.send_reset_request(now, 1)?;
        let c = reset_requests(&mut a, now).remove(0);
        let rsn = request(&c).reconfig_request_sequence_number;
        a.handle_reconfig_param(
            &response(rsn.wrapping_add(1), ReconfigResult::SuccessPerformed),
            &mut vec![],
            now,
        )?;
        assert!(a.reconfigs.contains_key(&rsn));
        a.handle_reconfig_param(&response(rsn, result), &mut vec![], now)?;
        assert!(a.streams.contains_key(&1));
        assert!(!a.streams.get(&1).unwrap().outgoing_reset_complete);
        assert!(a.open_stream(1, PayloadProtocolIdentifier::Binary).is_err());
    }
    Ok(())
}

#[test]
fn stream_reset_all_streams_waits_for_drain_and_retries() -> Result<()> {
    let (a, mut b) = reset_pair();
    let now = Instant::now();
    b.open_stream(2, PayloadProtocolIdentifier::Binary)?;
    let d = ChunkPayloadData {
        tsn: b.peer_last_tsn.wrapping_add(1),
        stream_identifier: 2,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"old readable data"),
        ..Default::default()
    };
    b.handle_data(&d)?;
    let p = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: a.my_next_rsn,
        reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
        sender_last_tsn: b.peer_last_tsn,
        stream_identifiers: vec![],
    }) as Box<dyn Param>;
    b.handle_reconfig_param(&p, &mut vec![], now)?;
    assert_eq!(b.reconfig_requests.len(), 1);
    assert!(!b.streams.get(&1).unwrap().incoming_reset_complete);
    assert_eq!(
        b.stream(2)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
        Bytes::from_static(b"old readable data")
    );
    assert!(b.reconfig_requests.is_empty());
    assert!(b.streams.values().all(|s| s.incoming_reset_complete));
    assert_eq!(
        reset_requests(&mut b, now)
            .iter()
            .filter(|c| c
                .param_a
                .as_ref()
                .unwrap()
                .as_any()
                .is::<ParamOutgoingResetRequest>())
            .count(),
        1
    );
    Ok(())
}

#[test]
fn stream_reset_simultaneous_close_has_no_extra_reciprocal_request() -> Result<()> {
    let (mut a, mut b) = reset_pair();
    let now = Instant::now();
    a.send_reset_request(now, 1)?;
    b.send_reset_request(now, 1)?;
    let ca = reset_requests(&mut a, now).remove(0);
    let cb = reset_requests(&mut b, now).remove(0);
    let ra = a.handle_reconfig(&cb, now)?;
    let rb = b.handle_reconfig(&ca, now)?;
    for (receiver, packets) in [(&mut a, rb), (&mut b, ra)] {
        for p in packets {
            for c in p.chunks {
                if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                    receiver.handle_reconfig(c, now)?;
                }
            }
        }
        assert!(!receiver.streams.contains_key(&1));
        assert!(reset_requests(receiver, now).is_empty());
    }
    Ok(())
}

#[test]
fn stream_reset_forward_tsn_completes_deferred_reset_without_more_data() -> Result<()> {
    let (a, mut b) = reset_pair();
    let now = Instant::now();
    let boundary = b.peer_last_tsn.wrapping_add(1);
    let p = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: a.my_next_rsn,
        reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
        sender_last_tsn: boundary,
        stream_identifiers: vec![1],
    }) as Box<dyn Param>;
    b.handle_reconfig_param(&p, &mut vec![], now)?;
    assert_eq!(b.reconfig_requests.len(), 1);
    b.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: boundary,
        streams: vec![],
    })?;
    assert!(
        b.reconfig_requests.is_empty(),
        "abandoned last TSN must release deferred reset"
    );
    assert!(b.streams.get(&1).unwrap().incoming_reset_complete);
    Ok(())
}

#[test]
fn stream_reset_deferred_data_metadata_and_duplicates_are_bounded() -> Result<()> {
    for len in [0, 1, 128] {
        let (_, mut b) = reset_pair();
        b.max_receive_buffer_size = 4096;
        b.streams.get_mut(&1).unwrap().incoming_reset_complete = true;
        let initial_credit = b.get_my_receiver_window_credit();
        for i in 1..=1000 {
            let d = ChunkPayloadData {
                tsn: b.peer_last_tsn.wrapping_add(i),
                stream_identifier: 1,
                beginning_fragment: true,
                ending_fragment: true,
                user_data: Bytes::from(vec![7; len]),
                ..Default::default()
            };
            b.handle_data(&d)?;
            let used = b.deferred_reset_bytes;
            b.handle_data(&d)?;
            assert_eq!(
                b.deferred_reset_bytes, used,
                "duplicate consumes no extra capacity"
            );
        }
        assert!(
            b.deferred_reset_data.len() < 1000,
            "metadata limits even empty DATA"
        );
        assert!(b.deferred_reset_bytes <= 4096);
        assert_eq!(
            b.get_my_receiver_window_credit() as usize + b.deferred_reset_bytes,
            initial_credit as usize
        );
    }
    Ok(())
}

#[test]
fn stream_reset_wraps_tsn_boundary_and_retries_denied_request() -> Result<()> {
    let (mut a, mut b) = reset_pair();
    let now = Instant::now();
    a.my_next_tsn = 0;
    a.send_reset_request(now, 1)?;
    let first = reset_requests(&mut a, now).remove(0);
    assert_eq!(request(&first).sender_last_tsn, u32::MAX);
    let rsn = request(&first).reconfig_request_sequence_number;
    a.handle_reconfig_param(&response(rsn, ReconfigResult::Denied), &mut vec![], now)?;
    a.send_reset_request(now, 1)?;
    let retry = reset_requests(&mut a, now).remove(0);
    assert_eq!(
        request(&retry).reconfig_request_sequence_number,
        rsn.wrapping_add(1)
    );
    b.peer_last_tsn = u32::MAX;
    b.handle_data(&ChunkPayloadData {
        tsn: 0,
        stream_identifier: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"TSN zero"),
        ..Default::default()
    })?;
    assert_eq!(b.peer_last_tsn, 0);
    Ok(())
}

#[test]
fn stream_reset_reciprocal_waits_behind_outgoing_fragment_tail() -> Result<()> {
    let (mut a, mut b) = reset_pair();
    let now = Instant::now();
    let ppi = PayloadProtocolIdentifier::Binary;
    let fragment_size = b.max_payload_size;
    b.stream(1)?.write_sctp(
        now,
        &Bytes::from(vec![8; fragment_size as usize * 2 + 1]),
        ppi,
    )?;
    a.send_reset_request(now, 1)?;
    let c = reset_requests(&mut a, now).remove(0);
    b.handle_reconfig(&c, now)?;
    b.cwnd = b.max_payload_size;
    let (head, resets) = b.pop_pending_data_chunks_to_send(now);
    assert_eq!(head.len(), 1);
    assert!(resets.is_empty());
    b.cwnd = u32::MAX;
    let (tail, resets) = b.pop_pending_data_chunks_to_send(now);
    assert_eq!(tail.len(), 2);
    assert_eq!(resets, vec![1]);
    assert!(tail.last().unwrap().ending_fragment);
    Ok(())
}

#[test]
fn stream_reset_final_read_does_not_drain_reopened_generation() -> Result<()> {
    let (a, mut b) = reset_pair();
    let now = Instant::now();
    let old = ChunkPayloadData {
        tsn: b.peer_last_tsn.wrapping_add(1),
        stream_identifier: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"last old payload"),
        ..Default::default()
    };
    b.handle_data(&old)?;
    b.send_reset_request(now, 1)?;
    let c = reset_requests(&mut b, now).remove(0);
    b.handle_reconfig_param(
        &response(
            request(&c).reconfig_request_sequence_number,
            ReconfigResult::SuccessPerformed,
        ),
        &mut vec![],
        now,
    )?;
    let p = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: a.my_next_rsn,
        reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
        sender_last_tsn: old.tsn,
        stream_identifiers: vec![1],
    }) as Box<dyn Param>;
    b.handle_reconfig_param(&p, &mut vec![], now)?;
    let new = ChunkPayloadData {
        tsn: old.tsn.wrapping_add(1),
        user_data: Bytes::from_static(b"new payload"),
        ..old
    };
    b.handle_data(&new)?;
    {
        let mut old_handle = b.stream(1)?;
        assert_eq!(
            old_handle.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"last old payload")
        );
        assert!(
            old_handle.read_sctp()?.is_none(),
            "old handle must stop at its generation boundary"
        );
        assert!(!old_handle.is_writable());
        old_handle.finish()?;
    }
    assert_eq!(
        b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
        Bytes::from_static(b"new payload")
    );
    assert!(b.stream(1)?.is_writable());
    assert!(b.accept_stream().is_some());
    assert!(b.accept_stream().is_none());
    Ok(())
}

#[test]
fn stream_reset_discards_old_readable_events_before_reopening() -> Result<()> {
    let (a, mut b) = reset_pair();
    let now = Instant::now();
    let first = b.peer_last_tsn.wrapping_add(1);
    for (offset, payload) in [b"first old".as_slice(), b"last old".as_slice()]
        .into_iter()
        .enumerate()
    {
        b.handle_data(&ChunkPayloadData {
            tsn: first.wrapping_add(offset as u32),
            stream_identifier: 1,
            stream_sequence_number: offset as u16,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::copy_from_slice(payload),
            ..Default::default()
        })?;
    }
    assert_eq!(
        b.events
            .iter()
            .filter(|e| matches!(e, Event::Stream(StreamEvent::Readable { id: 1 })))
            .count(),
        2
    );
    b.send_reset_request(now, 1)?;
    let c = reset_requests(&mut b, now).remove(0);
    b.handle_reconfig_param(
        &response(
            request(&c).reconfig_request_sequence_number,
            ReconfigResult::SuccessPerformed,
        ),
        &mut vec![],
        now,
    )?;
    let p = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: a.my_next_rsn,
        reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
        sender_last_tsn: first.wrapping_add(1),
        stream_identifiers: vec![1],
    }) as Box<dyn Param>;
    b.handle_reconfig_param(&p, &mut vec![], now)?;
    b.handle_data(&ChunkPayloadData {
        tsn: first.wrapping_add(2),
        stream_identifier: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"new"),
        ..Default::default()
    })?;
    {
        let mut old = b.stream(1)?;
        assert_eq!(
            old.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"first old")
        );
        assert_eq!(
            old.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"last old")
        );
        assert!(old.read_sctp()?.is_none());
    }
    let events: Vec<_> = std::iter::from_fn(|| b.poll()).collect();
    let close = events
        .iter()
        .position(|e| matches!(e, Event::AssociationLost { id: 1, .. }))
        .unwrap();
    let readable: Vec<_> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, Event::Stream(StreamEvent::Readable { id: 1 })))
        .collect();
    assert_eq!(
        readable.len(),
        1,
        "only the new generation readable event remains"
    );
    assert!(
        readable[0].0 > close,
        "close precedes the new generation's data"
    );
    assert_eq!(
        b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
        Bytes::from_static(b"new")
    );
    Ok(())
}

#[test]
fn stream_reset_stopped_reader_retries_deferred_incoming_reset() -> Result<()> {
    let (a, mut b) = reset_pair();
    let now = Instant::now();
    let d = ChunkPayloadData {
        tsn: b.peer_last_tsn.wrapping_add(1),
        stream_identifier: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"locally discarded"),
        ..Default::default()
    };
    b.handle_data(&d)?;
    let p = Box::new(ParamOutgoingResetRequest {
        reconfig_request_sequence_number: a.my_next_rsn,
        reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
        sender_last_tsn: d.tsn,
        stream_identifiers: vec![1],
    }) as Box<dyn Param>;
    b.handle_reconfig_param(&p, &mut vec![], now)?;
    assert_eq!(b.reconfig_requests.len(), 1);
    b.stream(1)?.close(now)?;
    assert!(
        b.reconfig_requests.is_empty(),
        "closing a reader must not wait forever for an impossible read"
    );
    assert!(b.streams.get(&1).unwrap().incoming_reset_complete);
    Ok(())
}

#[test]
fn stream_reset_busy_response_retries_same_request_sequence() -> Result<()> {
    let (mut a, _) = reset_pair();
    let now = Instant::now();
    a.send_reset_request(now, 1)?;
    let original = reset_requests(&mut a, now).remove(0);
    let rsn = request(&original).reconfig_request_sequence_number;
    a.handle_reconfig_param(
        &response(rsn, ReconfigResult::ErrorRequestAlreadyInProgress),
        &mut vec![],
        now,
    )?;
    assert!(
        a.reconfigs.contains_key(&rsn),
        "busy peer must retain the same request sequence"
    );
    a.open_stream(2, PayloadProtocolIdentifier::Binary)?;
    a.send_reset_request(now, 2)?;
    assert!(
        reset_requests(&mut a, now).is_empty(),
        "a second reset cannot bypass the busy request"
    );
    assert_eq!(a.reconfigs.len(), 1);
    assert!(
        !a.reconfig_in_progress,
        "busy retries retain the normal retransmission error budget"
    );
    let at = a
        .poll_timeout()
        .expect("busy response keeps retry timer armed");
    a.handle_timeout(at);
    let retransmit = reset_requests(&mut a, at).remove(0);
    assert_eq!(request(&retransmit), request(&original));
    Ok(())
}

// Issue #1233: busy replies must not replenish the configured reset retry budget.
#[test]
fn stream_reset_repeated_busy_responses_keep_retry_budget_and_backoff() -> Result<()> {
    for retries in [0, 1, 5] {
        let (mut a, _) = reset_pair();
        a.timers = TimerTable::new(TimerConfig {
            max_reconfig_retrans: retries,
            ..TimerConfig::default()
        });
        let now = Instant::now();
        a.events.clear();
        a.send_reset_request(now, 1)?;
        let original = reset_requests(&mut a, now).remove(0);
        let rsn = request(&original).reconfig_request_sequence_number;
        let busy = response(rsn, ReconfigResult::ErrorRequestAlreadyInProgress);
        let mut at = now;
        let mut interval = a.rto_mgr.get_rto();
        let mut observed_intervals = vec![];
        let mut expected_intervals = vec![];
        for attempt in 0..=retries {
            a.handle_reconfig_param(&busy, &mut vec![], at)?;
            let deadline = a.poll_timeout().expect("busy reset keeps recovery armed");
            observed_intervals.push(deadline.duration_since(at));
            expected_intervals.push(Duration::from_millis(interval));
            a.handle_timeout(deadline);
            at = deadline;
            if attempt < retries {
                let resent = reset_requests(&mut a, at);
                assert_eq!(resent.len(), 1, "budget={retries}, attempt={attempt}");
                assert_eq!(request(&resent[0]), request(&original));
                assert_eq!(a.state(), AssociationState::Established);
                interval = (interval * 2).min(60000);
            }
        }
        assert_eq!(a.state(), AssociationState::Closed, "budget={retries}");
        assert_eq!(observed_intervals, expected_intervals, "budget={retries}");
        assert!(a.poll_timeout().is_none());
        assert!(a.streams.is_empty());
        assert_eq!(
            a.events
                .iter()
                .filter(|event| matches!(
                    event,
                    Event::AssociationFailed {
                        reason: AssociationError::TimedOut
                    }
                ))
                .count(),
            1
        );
        a.events.clear();
        a.handle_timeout(at + Duration::from_secs(1000));
        a.handle_reconfig_param(&busy, &mut vec![], at)?;
        assert!(a.events.is_empty());
    }
    Ok(())
}

#[test]
fn stream_reset_busy_recovery_preserves_reopened_gameplay() -> Result<()> {
    let (mut a, mut b) = reset_pair();
    let ppi = PayloadProtocolIdentifier::Binary;
    let mut now = Instant::now();
    a.send_reset_request(now, 1)?;
    let original = reset_requests(&mut a, now).remove(0);
    let rsn = request(&original).reconfig_request_sequence_number;
    for _ in 0..2 {
        a.handle_reconfig_param(
            &response(rsn, ReconfigResult::ErrorRequestAlreadyInProgress),
            &mut vec![],
            now,
        )?;
        now = a.poll_timeout().expect("busy retry remains armed");
        a.handle_timeout(now);
        let retry = reset_requests(&mut a, now).remove(0);
        assert_eq!(request(&retry), request(&original));
    }
    for packet in b.handle_reconfig(&original, now)? {
        a.handle_inbound(packet, now)?;
    }
    for _ in 0..4 {
        for raw in b.gather_outbound(now).0 {
            a.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        for raw in a.gather_outbound(now).0 {
            b.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
    }
    assert!(!a.streams.contains_key(&1));
    assert!(!b.streams.contains_key(&1));
    assert!(a.timers.get(Timer::Reconfig).is_none());
    a.open_stream(1, ppi)?
        .write_sctp(now, &Bytes::from_static(b"fresh gameplay"), ppi)?;
    for raw in a.gather_outbound(now).0 {
        b.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    assert_eq!(
        b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
        Bytes::from_static(b"fresh gameplay")
    );
    assert!(b.stream(1)?.read_sctp()?.is_none());
    Ok(())
}

#[test]
fn stream_reset_duplicate_busy_responses_preserve_due_deadline() -> Result<()> {
    let (mut a, _) = reset_pair();
    a.timers = TimerTable::new(TimerConfig {
        max_reconfig_retrans: 1,
        ..TimerConfig::default()
    });
    let mut now = Instant::now();
    a.send_reset_request(now, 1)?;
    let original = reset_requests(&mut a, now).remove(0);
    let busy = response(
        request(&original).reconfig_request_sequence_number,
        ReconfigResult::ErrorRequestAlreadyInProgress,
    );
    for attempt in 0..2 {
        let deadline = a.poll_timeout().expect("reset recovery remains armed");
        for reply_at in [
            now + Duration::from_millis(1),
            deadline + Duration::from_millis(1),
        ] {
            a.handle_reconfig_param(&busy, &mut vec![], reply_at)?;
            assert_eq!(a.poll_timeout(), Some(deadline));
        }
        now = deadline + Duration::from_millis(1);
        a.handle_timeout(now);
        if attempt == 0 {
            let retried = reset_requests(&mut a, now);
            assert_eq!(retried.len(), 1);
            assert_eq!(request(&retried[0]), request(&original));
        }
    }
    assert_eq!(a.state(), AssociationState::Closed);
    assert!(a.poll_timeout().is_none());
    Ok(())
}

#[test]
fn stream_reset_held_forward_tsn_discards_incomplete_but_keeps_complete_data() -> Result<()> {
    for unordered in [false, true] {
        for complete in [false, true] {
            let (_, mut b) = reset_pair();
            b.streams.get_mut(&1).unwrap().incoming_reset_complete = true;
            let head = b.peer_last_tsn.wrapping_add(1);
            let d = ChunkPayloadData {
                tsn: head,
                stream_identifier: 1,
                unordered,
                beginning_fragment: true,
                ending_fragment: complete,
                user_data: Bytes::from_static(b"held head"),
                ..Default::default()
            };
            b.handle_data(&d)?;
            assert_eq!(b.create_selective_ack_chunk().cumulative_tsn_ack, head);
            let forward = ChunkForwardTsn {
                new_cumulative_tsn: head.wrapping_add(1),
                streams: if unordered {
                    vec![]
                } else {
                    vec![ChunkForwardTsnStream {
                        identifier: 1,
                        sequence: 0,
                    }]
                },
            };
            b.handle_forward_tsn(&forward)?;
            assert_eq!(
                b.deferred_reset_data.len(),
                usize::from(complete),
                "forwarding must remove incomplete saved fragments only"
            );
            b.unregister_stream(1, AssociationError::Reset);
            b.release_reset_data();
            if complete {
                let held = b.stream(1)?.read_sctp()?.unwrap();
                assert_eq!(
                    held.to_payload(100)?.freeze(),
                    Bytes::from_static(b"held head")
                );
            } else {
                assert!(
                    !b.streams.contains_key(&1),
                    "abandoned fragments must not reserve the freed SID"
                );
            }
            let next = ChunkPayloadData {
                tsn: forward.new_cumulative_tsn.wrapping_add(1),
                stream_identifier: 1,
                stream_sequence_number: 1,
                unordered,
                beginning_fragment: true,
                ending_fragment: true,
                user_data: Bytes::from_static(b"after abandonment"),
                ..Default::default()
            };
            b.handle_data(&next)?;
            assert_eq!(
                b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
                Bytes::from_static(b"after abandonment")
            );
            assert!(b.deferred_reset_data.is_empty());
            assert_eq!(b.deferred_reset_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn stream_reset_held_forward_tsn_without_data_preserves_ordered_ssn() -> Result<()> {
    let (_, mut b) = reset_pair();
    b.streams.get_mut(&1).unwrap().incoming_reset_complete = true;
    let skip = b.peer_last_tsn.wrapping_add(1);
    b.handle_forward_tsn(&ChunkForwardTsn {
        new_cumulative_tsn: skip,
        streams: vec![ChunkForwardTsnStream {
            identifier: 1,
            sequence: 0,
        }],
    })?;
    b.unregister_stream(1, AssociationError::Reset);
    b.release_reset_data();
    assert!(
        !b.streams.contains_key(&1),
        "a forwarded SSN marker must not reopen or reserve the SID"
    );
    assert!(b.accept_stream().is_none());
    b.open_stream(1, PayloadProtocolIdentifier::Binary)?;
    b.handle_data(&ChunkPayloadData {
        tsn: skip.wrapping_add(1),
        stream_identifier: 1,
        stream_sequence_number: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"first received new message"),
        ..Default::default()
    })?;
    assert_eq!(
        b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
        Bytes::from_static(b"first received new message")
    );
    Ok(())
}

// Issue #1184: a reset that completes while draining must keep future SSN progress.
#[test]
fn graceful_shutdown_reset_preserves_forwarded_future_sequence_without_data() -> Result<()> {
    for state in [
        AssociationState::ShutdownPending,
        AssociationState::ShutdownReceived,
        AssociationState::ShutdownSent,
        AssociationState::ShutdownAckSent,
    ] {
        let (a, mut b) = reset_pair();
        let now = Instant::now();
        let old_tsn = b.peer_last_tsn.wrapping_add(1);
        b.handle_data(&ChunkPayloadData {
            tsn: old_tsn,
            stream_identifier: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"old final message"),
            ..Default::default()
        })?;
        b.streams.get_mut(&1).unwrap().outgoing_reset_complete = true;
        let reset = Box::new(ParamOutgoingResetRequest {
            reconfig_request_sequence_number: a.my_next_rsn,
            reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
            sender_last_tsn: old_tsn,
            stream_identifiers: vec![1],
        }) as Box<dyn Param>;
        b.handle_reconfig_param(&reset, &mut vec![], now)?;
        b.set_state(state);
        let skipped = old_tsn.wrapping_add(1);
        b.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn: skipped,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: 0,
            }],
        })?;
        assert!(b.deferred_reset_reassembly.is_empty());
        assert_eq!(
            b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"old final message")
        );
        assert!(!b.streams.contains_key(&1));
        for sequence in 1..=2 {
            b.handle_data(&ChunkPayloadData {
                tsn: skipped.wrapping_add(u32::from(sequence)),
                stream_identifier: 1,
                stream_sequence_number: sequence,
                beginning_fragment: true,
                ending_fragment: true,
                user_data: Bytes::from_static(b"admitted future tail"),
                ..Default::default()
            })?;
        }
        assert_eq!(b.peer_last_tsn, skipped.wrapping_add(2));
        assert_eq!(
            b.stream(1)?
                .read_sctp()?
                .expect("accepted SSN 1 must not wait for abandoned SSN 0")
                .to_payload(100)?
                .freeze(),
            Bytes::from_static(b"admitted future tail")
        );
        assert!(b.reset_ssn_markers.is_empty());
        assert!(!b.stream(1)?.is_writable());
        assert!(matches!(
            b.stream(1)?.write_sctp(
                now,
                &Bytes::from_static(b"late local write"),
                PayloadProtocolIdentifier::Binary
            ),
            Err(Error::ErrStreamClosed)
        ));
        b.close(AssociationError::AssociationClosed)?;
        b.endpoint_events.push_back(EndpointEventInner::Drained);
        assert!(b.poll_endpoint_event().is_none());
        let mut reader = b
            .accept_stream()
            .expect("the accepted future tail stays readable");
        assert_eq!(
            reader.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"admitted future tail")
        );
        assert!(b.poll_endpoint_event().is_some());
        assert!(b.poll_endpoint_event().is_none());
        assert!(b.streams.is_empty());
        assert!(b.reset_ssn_markers.is_empty());
        assert!(b.open_stream(1, PayloadProtocolIdentifier::Binary).is_err());
    }
    Ok(())
}

#[test]
fn stream_reset_pending_old_drain_forwards_future_ordered_queue() -> Result<()> {
    for future_id in [1, 2] {
        let (a, mut b) = reset_pair();
        let now = Instant::now();
        let old_tsn = b.peer_last_tsn.wrapping_add(1);
        b.handle_data(&ChunkPayloadData {
            tsn: old_tsn,
            stream_identifier: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"old final message"),
            ..Default::default()
        })?;
        b.streams.get_mut(&1).unwrap().outgoing_reset_complete = true;
        let p = Box::new(ParamOutgoingResetRequest {
            reconfig_request_sequence_number: a.my_next_rsn,
            reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
            sender_last_tsn: old_tsn,
            stream_identifiers: if future_id == 1 { vec![1] } else { vec![] },
        }) as Box<dyn Param>;
        b.handle_reconfig_param(&p, &mut vec![], now)?;
        assert!(!b.streams.get(&1).unwrap().incoming_reset_complete);
        let head = old_tsn.wrapping_add(1);
        if future_id == 1 {
            b.handle_data(&ChunkPayloadData {
                tsn: head,
                stream_identifier: future_id,
                beginning_fragment: true,
                ending_fragment: false,
                user_data: Bytes::from_static(b"incomplete future"),
                ..Default::default()
            })?;
        }
        b.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn: head.wrapping_add(1),
            streams: vec![ChunkForwardTsnStream {
                identifier: future_id,
                sequence: 0,
            }],
        })?;
        if future_id == 1 {
            assert_eq!(
                b.deferred_reset_reassembly
                    .get(&future_id)
                    .unwrap()
                    .next_ssn,
                1,
                "future ordered progress must not wait for the old application's drain"
            );
        } else {
            assert_eq!(
                b.reset_ssn_markers.get(&future_id),
                Some(&1),
                "a future SID without DATA must save its ordered progress lazily"
            );
        }
        b.handle_data(&ChunkPayloadData {
            tsn: head.wrapping_add(2),
            stream_identifier: future_id,
            stream_sequence_number: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"future complete message"),
            ..Default::default()
        })?;
        {
            let mut old = b.stream(1)?;
            assert_eq!(
                old.read_sctp()?.unwrap().to_payload(100)?.freeze(),
                Bytes::from_static(b"old final message")
            );
            assert!(old.read_sctp()?.is_none());
        }
        assert_eq!(
            b.stream(future_id)?
                .read_sctp()?
                .unwrap()
                .to_payload(100)?
                .freeze(),
            Bytes::from_static(b"future complete message")
        );
        assert!(b.stream(future_id)?.read_sctp()?.is_none());
    }
    Ok(())
}

#[test]
fn stream_reset_held_fragments_cross_tsn_wrap() -> Result<()> {
    for unordered in [false, true] {
        let (_, mut b) = reset_pair();
        b.peer_last_tsn = u32::MAX - 1;
        b.streams.get_mut(&1).unwrap().incoming_reset_complete = true;
        for (tsn, beginning, ending, data) in [
            (u32::MAX, true, false, b"head".as_slice()),
            (0, false, true, b"tail".as_slice()),
        ] {
            b.handle_data(&ChunkPayloadData {
                tsn,
                stream_identifier: 1,
                unordered,
                beginning_fragment: beginning,
                ending_fragment: ending,
                user_data: Bytes::copy_from_slice(data),
                ..Default::default()
            })?;
        }
        assert_eq!(b.create_selective_ack_chunk().cumulative_tsn_ack, 0);
        b.unregister_stream(1, AssociationError::Reset);
        b.release_reset_data();
        assert_eq!(
            b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"headtail")
        );
        assert!(b.stream(1)?.read_sctp()?.is_none());
    }
    Ok(())
}

// The real sender's incremental FWD map must not carry an old SSN across reset.
#[test]
fn stream_reset_forward_map_generation_boundary_on_wire() -> Result<()> {
    for (old_data_delivered, fresh_zero_delivered) in
        [(false, false), (false, true), (true, false), (true, true)]
    {
        let (mut a, mut b) = reset_pair();
        let now = Instant::now();
        let ppi = PayloadProtocolIdentifier::Binary;
        a.open_stream(2, ppi)?;
        b.open_stream(2, ppi)?;
        // Establish a genuinely nonzero old SSN using sent, read and SACKed DATA.
        for _ in 0..5 {
            a.stream(1)?
                .write_sctp(now, &Bytes::from_static(b"old"), ppi)?;
            for d in transmitted_data(&a.gather_outbound(now).0) {
                b.handle_data(&d)?;
            }
            assert!(b.stream(1)?.read_sctp()?.is_some());
            a.handle_sack(&b.create_selective_ack_chunk(), now)?;
        }
        a.stream(1)?
            .set_reliability_params(false, ReliabilityType::Timed, 1)?;
        a.stream(1)?
            .write_sctp(now, &Bytes::from_static(b"old SSN five"), ppi)?;
        let old = transmitted_data(&a.gather_outbound(now).0).remove(0);
        assert_eq!(old.stream_sequence_number, 5);
        if old_data_delivered {
            b.handle_data(&old)?;
            assert!(b.stream(1)?.read_sctp()?.is_some());
        }
        // Withhold every further SACK: reset success is independent of sender cumACK.
        a.send_reset_request(now, 1)?;
        let outgoing = reset_requests(&mut a, now).remove(0);
        let boundary = request(&outgoing).sender_last_tsn;
        let mut replies = b.handle_reconfig(&outgoing, now)?;
        a.stream(2)?
            .set_reliability_params(false, ReliabilityType::Timed, 1)?;
        a.stream(2)?
            .write_sctp(now, &Bytes::from_static(b"other SID abandoned"), ppi)?;
        let other = transmitted_data(&a.gather_outbound(now).0).remove(0);
        assert!(sna32gt(other.tsn, boundary));
        let later = now + Duration::from_secs(2);
        if !old_data_delivered {
            a.handle_timeout(later);
            let forwarded = a.gather_outbound(later).0;
            let fwd = forwarded
                .iter()
                .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
                .find_map(|c| c.as_any().downcast_ref::<ChunkForwardTsn>().cloned())
                .unwrap();
            assert_eq!(
                fwd.new_cumulative_tsn, boundary,
                "pending reset must fence the association-wide FWD boundary"
            );
            assert_eq!(
                fwd.streams.len(),
                1,
                "other SID above the fence must not be skipped"
            );
            assert_eq!(fwd.streams[0].identifier, 1);
            assert_eq!(fwd.streams[0].sequence, 5);
            replies.extend(b.handle_forward_tsn(&fwd)?);
        }
        for packet in replies {
            for c in packet.chunks {
                if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                    a.handle_reconfig(c, later)?;
                }
            }
        }
        let reciprocal = reset_requests(&mut b, later).remove(0);
        let final_ack = a.handle_reconfig(&reciprocal, later)?;
        assert!(!a.streams.contains_key(&1));
        if old_data_delivered {
            a.handle_timeout(later);
        }
        assert!(a.retired_reset_tsns.contains_key(&1));
        a.open_stream(1, ppi)?
            .set_reliability_params(false, ReliabilityType::Timed, 1)?;
        a.stream(1)?
            .write_sctp(later, &Bytes::from_static(b"fresh zero"), ppi)?;
        let raw = a.gather_outbound(later).0;
        let fresh = transmitted_data(&raw).remove(0);
        assert_eq!(fresh.stream_sequence_number, 0);
        if fresh_zero_delivered {
            b.handle_data(&fresh)?; // before final reset ACK: held new generation
        }
        // Retransmitted FWD state after reset must retain only the other SID.
        a.will_send_forward_tsn = true;
        let forwarded = a.gather_outbound(later).0;
        let fwd = forwarded
            .iter()
            .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
            .find_map(|c| c.as_any().downcast_ref::<ChunkForwardTsn>().cloned())
            .unwrap();
        assert_eq!(fwd.new_cumulative_tsn, other.tsn);
        assert!(
            fwd.streams.iter().all(|s| s.identifier != 1),
            "old abandoned chunks and cached SSNs must not re-enter the new generation"
        );
        b.handle_forward_tsn(&fwd)?;
        let future = later + Duration::from_secs(10);
        a.handle_timeout(future); // abandon genuine SSN0 in the new generation
        let raw = a.gather_outbound(future).0;
        let fresh_fwd = raw
            .iter()
            .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
            .find_map(|c| c.as_any().downcast_ref::<ChunkForwardTsn>().cloned())
            .unwrap();
        assert_eq!(fresh_fwd.new_cumulative_tsn, fresh.tsn);
        assert_eq!(
            fresh_fwd
                .streams
                .iter()
                .find(|s| s.identifier == 1)
                .unwrap()
                .sequence,
            0
        );
        b.handle_forward_tsn(&fresh_fwd)?;
        a.stream(1)?
            .set_reliability_params(false, ReliabilityType::Reliable, 0)?;
        a.stream(1)?
            .write_sctp(future, &Bytes::from_static(b"fresh one"), ppi)?;
        let next = transmitted_data(&a.gather_outbound(future).0).remove(0);
        assert_eq!(next.stream_sequence_number, 1);
        b.handle_data(&next)?;
        for packet in final_ack {
            for c in packet.chunks {
                if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                    b.handle_reconfig(c, later)?;
                }
            }
        }
        let expected: &[&[u8]] = if fresh_zero_delivered {
            &[b"fresh zero", b"fresh one"]
        } else {
            &[b"fresh one"]
        };
        for &expected in expected {
            assert_eq!(
                b.stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(100)?
                    .freeze()
                    .as_ref(),
                expected
            );
        }
        assert!(b.stream(1)?.read_sctp()?.is_none());
        a.handle_sack(&b.create_selective_ack_chunk(), future)?;
        assert!(a.retired_reset_tsns.is_empty());
    }
    Ok(())
}

#[test]
fn stream_reset_fence_resumes_forward_recovery_after_long_wait() -> Result<()> {
    for (result, old_sack_received) in [
        (ReconfigResult::SuccessPerformed, true),
        (ReconfigResult::Denied, true),
        (ReconfigResult::SuccessPerformed, false),
        (ReconfigResult::Denied, false),
    ] {
        let (mut a, mut b) = reset_pair();
        let ppi = PayloadProtocolIdentifier::Binary;
        let now = Instant::now();
        a.stream(1)?
            .set_reliability_params(false, ReliabilityType::Timed, 1)?;
        a.stream(1)?
            .write_sctp(now, &Bytes::from_static(b"old reader held"), ppi)?;
        let old = transmitted_data(&a.gather_outbound(now).0).remove(0);
        b.handle_data(&old)?;
        if old_sack_received {
            a.handle_sack(&b.create_selective_ack_chunk(), now)?;
        }
        a.send_reset_request(now, 1)?;
        let outgoing = reset_requests(&mut a, now).remove(0);
        let rsn = request(&outgoing).reconfig_request_sequence_number;
        for packet in b.handle_reconfig(&outgoing, now)? {
            for c in packet.chunks {
                if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                    a.handle_reconfig(c, now)?;
                }
            }
        }
        assert!(a.reconfig_in_progress);
        a.open_stream(2, ppi)?
            .set_reliability_params(false, ReliabilityType::Timed, 1)?;
        b.open_stream(2, ppi)?;
        a.stream(2)?
            .write_sctp(now, &Bytes::from_static(b"lost other SID"), ppi)?;
        let missing = transmitted_data(&a.gather_outbound(now).0).remove(0);
        let mut at = now;
        let mut paused_rto = None;
        for turn in 0..if old_sack_received { 8 } else { 5 } {
            // beyond the published five-expiry T3 budget
            at = a
                .timers
                .get(Timer::T3RTX)
                .unwrap_or(at + Duration::from_secs(60));
            a.handle_timeout(at);
            let raw = a.gather_outbound(at).0;
            for c in raw
                .iter()
                .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
            {
                if let Some(fwd) = c.as_any().downcast_ref::<ChunkForwardTsn>() {
                    assert!(!old_sack_received);
                    assert_eq!(
                        fwd.new_cumulative_tsn, old.tsn,
                        "pending reset caps recovery at old boundary despite a withheld SACK"
                    );
                }
            }
            if turn == 0 {
                paused_rto = Some((a.rto_mgr.get_rto(), a.cwnd));
            } else if old_sack_received {
                assert_eq!(
                    Some((a.rto_mgr.get_rto(), a.cwnd)),
                    paused_rto,
                    "local reset wait must not back off DATA RTO or shrink cwnd"
                );
            }
            a.handle_reconfig_param(&response(rsn, ReconfigResult::InProgress), &mut vec![], at)?;
        }
        let preserved_deadline = a.timers.get(Timer::T3RTX);
        a.handle_reconfig_param(&response(rsn, result), &mut vec![], at)?;
        if let Some(deadline) = preserved_deadline {
            assert_eq!(
                a.timers.get(Timer::T3RTX),
                Some(deadline),
                "reset response preserves existing T3 deadline"
            );
        }
        let first = a.gather_outbound(at).0;
        let parse_fwd = |raw: &[Bytes]| {
            raw.iter()
                .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
                .find_map(|c| c.as_any().downcast_ref::<ChunkForwardTsn>().cloned())
        };
        let released =
            parse_fwd(&first).expect("lifting reset fence sends outstanding forward debt");
        assert_eq!(released.new_cumulative_tsn, missing.tsn);
        // Lose that first released FWD. Recovery must have a live, usable timer.
        let retry_at = a
            .timers
            .get(Timer::T3RTX)
            .expect("reset completion must rearm forward recovery");
        a.handle_timeout(retry_at);
        let retried =
            parse_fwd(&a.gather_outbound(retry_at).0).expect("lost lifted FWD must retransmit");
        assert_eq!(retried.new_cumulative_tsn, missing.tsn);
        b.handle_forward_tsn(&retried)?;
        a.handle_sack(&b.create_selective_ack_chunk(), retry_at)?;
        assert!(a.inflight_queue.is_empty());
        assert!(a.timers.get(Timer::T3RTX).is_none());
    }
    Ok(())
}

// Issue #1120: distinguish lost reset traffic from lost accepted DATA. These
// losses are independent even when encrypted native traffic cannot name them.
#[test]
fn reciprocal_reset_recovers_lost_request_data_and_final_response() -> Result<()> {
    for reliable in [false, true] {
        for wrap in [false, true] {
            let context = format!("reliable={reliable}, wrap={wrap}");
            let (mut local, mut remote) = reset_pair();
            let now = Instant::now();
            let ppi = PayloadProtocolIdentifier::Binary;
            if wrap {
                local.my_next_tsn = u32::MAX;
                local.cumulative_tsn_ack_point = u32::MAX - 1;
                local.advanced_peer_tsn_ack_point = u32::MAX - 1;
                remote.peer_last_tsn = u32::MAX - 1;
            }
            local.stream(1)?.set_reliability_params(
                !reliable,
                if reliable {
                    ReliabilityType::Reliable
                } else {
                    ReliabilityType::Rexmit
                },
                0,
            )?;
            local
                .stream(1)?
                .write_sctp(now, &Bytes::from_static(b"x"), ppi)?;
            let lost_data = transmitted_data(&local.gather_outbound(now).0);
            assert_eq!(lost_data.len(), 1, "{context}");
            remote.send_reset_request(now, 1)?;
            let lost_reset = reset_requests(&mut remote, now).remove(0);
            assert_eq!(local.buffered_amount(), 1, "{context}");
            assert!(!local.streams[&1].incoming_reset_complete, "{context}");
            assert!(remote.streams[&1].reset_requested, "{context}");
            assert_eq!(remote.buffered_amount(), 0, "{context}");

            let reset_at = remote.timers.get(Timer::Reconfig).unwrap();
            remote.handle_timeout(reset_at);
            let mut retries = reset_requests(&mut remote, reset_at);
            assert_eq!(
                retries.len(),
                1,
                "{context}: lost remote reset must retransmit at its deadline"
            );
            let retry = retries.remove(0);
            assert_eq!(request(&retry), request(&lost_reset), "{context}");
            // The first success response is delivered. The reciprocal reset
            // still waits for the missing local DATA's transport boundary.
            for packet in local.handle_reconfig(&retry, reset_at)? {
                for c in packet.chunks {
                    if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                        remote.handle_reconfig(c, reset_at)?;
                    }
                }
            }
            let reciprocal = reset_requests(&mut local, reset_at).remove(0);
            let reciprocal_rsn = request(&reciprocal).reconfig_request_sequence_number;
            for packet in remote.handle_reconfig(&reciprocal, reset_at)? {
                for c in packet.chunks {
                    if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                        local.handle_reconfig(c, reset_at)?;
                    }
                }
            }
            assert!(local.reconfig_in_progress, "{context}");
            assert!(remote.streams[&1].outgoing_reset_complete, "{context}");
            assert!(!remote.streams[&1].incoming_reset_complete, "{context}");

            let data_at = local.timers.get(Timer::T3RTX).unwrap();
            assert!(
                data_at >= reset_at,
                "{context}: clock must not move backward"
            );
            local.handle_timeout(data_at);
            let recovery = local.gather_outbound(data_at).0;
            let mut terminal = vec![];
            let mut recovery_counts = (0, 0);
            for raw in &recovery {
                for c in Packet::unmarshal(raw)?.chunks {
                    if let Some(c) = c.as_any().downcast_ref::<ChunkPayloadData>() {
                        recovery_counts.0 += 1;
                        assert_eq!(c.tsn, lost_data[0].tsn, "{context}");
                        assert!(
                            reliable,
                            "{context}: zero-retransmit DATA must not be sent twice"
                        );
                        remote.handle_data(c)?;
                    } else if let Some(c) = c.as_any().downcast_ref::<ChunkForwardTsn>() {
                        recovery_counts.1 += 1;
                        assert_eq!(c.new_cumulative_tsn, lost_data[0].tsn, "{context}");
                        assert!(!reliable, "{context}: reliable DATA must not be abandoned");
                        terminal.extend(remote.handle_forward_tsn(c)?);
                    }
                }
            }
            assert_eq!(
                recovery_counts,
                if reliable { (1, 0) } else { (0, 1) },
                "{context}"
            );
            if reliable {
                assert_eq!(
                    remote
                        .stream(1)?
                        .read_sctp()?
                        .unwrap()
                        .to_payload(10)?
                        .freeze(),
                    Bytes::from_static(b"x"),
                    "{context}"
                );
            }
            terminal.extend(remote.control_queue.drain(..));
            // Lose only the terminal reply to the reciprocal request. No
            // further DATA or application read may be needed for recovery.
            assert_eq!(
                terminal.len(),
                1,
                "{context}: lose exactly one terminal packet"
            );
            assert_eq!(terminal[0].chunks.len(), 1, "{context}");
            let lost_terminal = terminal[0].chunks[0]
                .as_any()
                .downcast_ref::<ChunkReconfig>()
                .unwrap()
                .param_a
                .as_ref()
                .unwrap()
                .as_any()
                .downcast_ref::<ParamReconfigResponse>()
                .unwrap();
            assert_eq!(
                lost_terminal.reconfig_response_sequence_number, reciprocal_rsn,
                "{context}"
            );
            assert_eq!(
                lost_terminal.result,
                ReconfigResult::SuccessPerformed,
                "{context}"
            );
            assert!(!remote.streams.contains_key(&1), "{context}");
            assert!(local.streams.contains_key(&1), "{context}");
            let retry_at = local.timers.get(Timer::Reconfig).unwrap();
            assert!(
                retry_at >= data_at,
                "{context}: clock must not move backward"
            );
            local.handle_timeout(retry_at);
            let mut retries = reset_requests(&mut local, retry_at);
            assert_eq!(
                retries.len(),
                1,
                "{context}: lost terminal reply must cause a retry"
            );
            let retry = retries.remove(0);
            assert_eq!(request(&retry), request(&reciprocal), "{context}");
            for packet in remote.handle_reconfig(&retry, retry_at)? {
                for c in packet.chunks {
                    if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                        local.handle_reconfig(c, retry_at)?;
                    }
                }
            }
            assert!(!local.streams.contains_key(&1), "{context}");
            for endpoint in [&local, &remote] {
                assert!(endpoint.reconfigs.is_empty(), "{context}");
                assert!(endpoint.timers.get(Timer::Reconfig).is_none(), "{context}");
                assert_eq!(endpoint.state(), AssociationState::Established, "{context}");
            }
            assert!(
                remote.pending_queue.is_empty(),
                "{context}: duplicate reset must not queue another reciprocal"
            );
            local.handle_sack(&remote.create_selective_ack_chunk(), retry_at)?;
            assert_eq!(local.buffered_amount(), 0, "{context}");
            local
                .open_stream(1, ppi)?
                .write_sctp(retry_at, &Bytes::from_static(b"fresh"), ppi)?;
            remote.open_stream(1, ppi)?;
            for c in transmitted_data(&local.gather_outbound(retry_at).0) {
                remote.handle_data(&c)?;
            }
            assert_eq!(
                remote
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(10)?
                    .freeze(),
                Bytes::from_static(b"fresh"),
                "{context}"
            );
            assert!(remote.stream(1)?.read_sctp()?.is_none(), "{context}");
        }
    }
    Ok(())
}

// Issue #1133: lost reset traffic must not leave a timerless SID.
#[test]
fn stream_reset_retry_exhaustion_closes_all_streams_once() -> Result<()> {
    for retries in [0, 1, 5] {
        let (mut a, _) = reset_pair();
        a.timers = TimerTable::new(TimerConfig {
            max_reconfig_retrans: retries,
            ..TimerConfig::default()
        });
        let now = Instant::now();
        a.open_stream(2, PayloadProtocolIdentifier::Binary)?;
        a.events.clear();
        a.send_reset_request(now, 1)?;
        let original = reset_requests(&mut a, now).remove(0);
        for _ in 0..retries {
            let at = a.poll_timeout().expect("reset retry remains armed");
            a.handle_timeout(at);
            let retransmitted = reset_requests(&mut a, at);
            assert_eq!(retransmitted.len(), 1, "budget={retries}");
            assert_eq!(request(&retransmitted[0]), request(&original));
            assert_eq!(a.state(), AssociationState::Established);
        }
        let at = a
            .poll_timeout()
            .expect("final reset expiration remains armed");
        a.handle_timeout(at);
        assert_eq!(a.state(), AssociationState::Closed, "budget={retries}");
        assert!(a.streams.is_empty());
        assert!(a.poll_timeout().is_none());
        let terminal = a
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::AssociationFailed {
                        reason: AssociationError::TimedOut
                    }
                )
            })
            .count();
        assert_eq!(terminal, 1);
        let closed: Vec<_> = a
            .events
            .drain(..)
            .filter_map(|event| {
                if let Event::AssociationLost { reason, id } = event {
                    assert_eq!(reason, AssociationError::TimedOut);
                    Some(id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(closed.len(), 2);
        assert!(closed.contains(&1) && closed.contains(&2));
        a.handle_timeout(at + Duration::from_secs(1000));
        assert!(
            a.poll().is_none(),
            "terminal timeout must notify each stream once"
        );
    }
    Ok(())
}

// Regression #1138: delayed gameplay cannot revive a retired association.
#[test]
fn terminal_association_ignores_delayed_and_bundled_gameplay() -> Result<()> {
    for history in [
        "reset exhaustion",
        "local abort",
        "abort before data",
        "data before abort",
    ] {
        let (mut a, mut b) = reset_pair();
        let now = Instant::now();
        b.stream(1)?.write_sctp(
            now,
            &Bytes::from_static(b"delayed gameplay"),
            PayloadProtocolIdentifier::Binary,
        )?;
        let data = transmitted_data(&b.gather_outbound(now).0).remove(0);
        let mut chunks: Vec<Box<dyn Chunk>> = vec![];
        match history {
            "reset exhaustion" => {
                a.timers = TimerTable::new(TimerConfig {
                    max_reconfig_retrans: 0,
                    ..TimerConfig::default()
                });
                a.send_reset_request(now, 1)?;
                assert_eq!(reset_requests(&mut a, now).len(), 1);
                a.handle_timeout(a.poll_timeout().expect("reset deadline"));
            }
            "local abort" => {
                a.abort()?;
                a.gather_outbound(now); // Deliver the local ABORT before delayed input.
            }
            _ => chunks.push(Box::new(ChunkAbort {
                error_causes: vec![ErrorCause {
                    code: USER_INITIATED_ABORT,
                    ..Default::default()
                }],
            })),
        }
        a.events.clear();
        if history == "data before abort" {
            chunks.insert(0, Box::new(data));
        } else {
            chunks.push(Box::new(data));
        }
        let wire = b.create_packet(chunks).marshal()?;
        a.handle_inbound(Packet::unmarshal(&wire)?, now)?;
        assert_eq!(a.state(), AssociationState::Closed, "{history}");
        assert!(
            a.streams.is_empty(),
            "{history}: late DATA recreated a stream"
        );
        assert!(
            a.accept_stream().is_none(),
            "{history}: late stream accepted"
        );
        assert!(
            a.poll_timeout().is_none(),
            "{history}: late input armed a timer"
        );
        assert!(
            a.gather_outbound(now).0.is_empty(),
            "{history}: late input emitted output"
        );
        assert!(
            !a.events.iter().any(|event| {
                matches!(event, Event::Connected | Event::Stream(_))
                    || (history != "data before abort" && matches!(event, Event::DatagramReceived))
            }),
            "{history}: late input emitted a live event"
        );
    }
    Ok(())
}

#[test]
fn terminal_association_ignores_old_handshake_but_fresh_server_connects() -> Result<()> {
    for delayed in ["INIT", "COOKIE-ECHO"] {
        let mut server = Association::default();
        let now = Instant::now();
        let init = ChunkInit {
            initiate_tag: 123,
            advertised_receiver_window_credit: 65536,
            num_outbound_streams: 1,
            num_inbound_streams: 1,
            initial_tsn: 42,
            ..Default::default()
        };
        let packet = |chunk: Box<dyn Chunk>| Packet {
            common_header: CommonHeader {
                source_port: 5000,
                destination_port: 5000,
                verification_tag: 0,
            },
            chunks: vec![chunk],
        };
        // A fresh server starts Closed and must still accept its first handshake.
        server.handle_inbound(packet(Box::new(init.clone())), now)?;
        assert!(!server.gather_outbound(now).0.is_empty());
        let echo = ChunkCookieEcho {
            cookie: server
                .my_cookie
                .as_ref()
                .expect("INIT creates cookie")
                .cookie
                .clone(),
        };
        server.handle_inbound(packet(Box::new(echo.clone())), now)?;
        assert_eq!(server.state(), AssociationState::Established);
        assert!(matches!(server.poll(), Some(Event::Connected)));
        server.gather_outbound(now);
        server.close(AssociationError::LocallyClosed)?;
        let late: Box<dyn Chunk> = if delayed == "INIT" {
            Box::new(init)
        } else {
            Box::new(echo)
        };
        let wire = packet(late).marshal()?;
        server.handle_inbound(Packet::unmarshal(&wire)?, now)?;
        assert_eq!(server.state(), AssociationState::Closed, "{delayed}");
        assert!(server.poll().is_none(), "{delayed}: no second connection");
        assert!(server.poll_timeout().is_none(), "{delayed}: no timer");
        assert!(
            server.gather_outbound(now).0.is_empty(),
            "{delayed}: no reply"
        );
    }
    Ok(())
}

// Issue #1143: gameplay teardown must use SHUTDOWN's cumulative acknowledgment.
#[test]
fn shutdown_acknowledges_received_gameplay_without_a_final_sack() -> Result<()> {
    for (wrap, simultaneous) in [(false, false), (false, true), (true, false), (true, true)] {
        for ppi in [
            PayloadProtocolIdentifier::Binary,
            PayloadProtocolIdentifier::String,
        ] {
            let (mut local, mut remote) = reset_pair();
            let now = Instant::now();
            if wrap {
                local.my_next_tsn = u32::MAX;
                local.cumulative_tsn_ack_point = u32::MAX - 1;
                local.advanced_peer_tsn_ack_point = u32::MAX - 1;
                remote.peer_last_tsn = u32::MAX - 1;
            }
            for text in [b"one".as_slice(), b"two".as_slice()] {
                local
                    .stream(1)?
                    .write_sctp(now, &Bytes::copy_from_slice(text), ppi)?;
            }
            let data = transmitted_data(&local.gather_outbound(now).0);
            assert_eq!(data.len(), 2);
            for chunk in &data {
                remote.handle_data(chunk)?;
            }
            // Drop every SACK. SHUTDOWN alone acknowledges the received DATA.
            remote.gather_outbound(now);
            remote.shutdown()?;
            let shutdown = remote
                .gather_outbound(now)
                .0
                .iter()
                .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
                .find_map(|c| c.as_any().downcast_ref::<ChunkShutdown>().cloned())
                .expect("remote emits SHUTDOWN");
            assert_eq!(
                shutdown.cumulative_tsn_ack, data[1].tsn,
                "SHUTDOWN must report peer DATA, wrap={wrap}, ppi={ppi:?}"
            );
            if simultaneous {
                local.shutdown()?;
            }
            let shutdown: Box<dyn Chunk> = Box::new(shutdown);
            local.handle_chunk(&local.create_packet(vec![]), &shutdown, now)?;
            assert_eq!(local.state(), AssociationState::ShutdownAckSent);
            assert!(local.inflight_queue.is_empty());
            assert!(local.timers.get(Timer::T3RTX).is_none());
            assert_eq!(local.stream(1)?.buffered_amount()?, 0);
            let released: usize = std::iter::from_fn(|| local.poll())
                .filter_map(|event| {
                    if let Event::Stream(StreamEvent::BufferedAmountReleased {
                        data_channel_bytes,
                        ..
                    }) = event
                    {
                        Some(data_channel_bytes)
                    } else {
                        None
                    }
                })
                .sum();
            assert_eq!(released, 6);
            // Lose the first SHUTDOWN ACK. A repeated peer SHUTDOWN replays it
            // without another DATA release or any DATA retransmission.
            let lost_ack = local.gather_outbound(now).0;
            assert!(
                lost_ack
                    .iter()
                    .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
                    .any(|c| c.as_any().is::<ChunkShutdownAck>())
            );
            let retry_at = remote
                .timers
                .get(Timer::T2Shutdown)
                .expect("peer shutdown retry stays armed");
            remote.handle_timeout(retry_at);
            let retry = remote
                .gather_outbound(retry_at)
                .0
                .iter()
                .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
                .find_map(|c| c.as_any().downcast_ref::<ChunkShutdown>().cloned())
                .expect("lost SHUTDOWN ACK causes another SHUTDOWN");
            assert_eq!(retry.cumulative_tsn_ack, data[1].tsn);
            let retry: Box<dyn Chunk> = Box::new(retry);
            local.handle_chunk(&local.create_packet(vec![]), &retry, retry_at)?;
            assert!(!std::iter::from_fn(|| local.poll()).any(|event| matches!(
                event,
                Event::Stream(StreamEvent::BufferedAmountReleased { .. })
            )));
            let ack = local
                .gather_outbound(retry_at)
                .0
                .iter()
                .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
                .find_map(|c| c.as_any().downcast_ref::<ChunkShutdownAck>().cloned())
                .expect("local emits SHUTDOWN ACK without a retry");
            remote.handle_shutdown_ack(&ack)?;
            assert!(
                !remote.gather_outbound(retry_at).1,
                "remote emits SHUTDOWN COMPLETE"
            );
            local.handle_shutdown_complete(&ChunkShutdownComplete::default())?;
            assert_eq!(local.state(), AssociationState::Closed);
        }
    }
    Ok(())
}

// Issue #1143: cumulative shutdown credit cannot acknowledge gaps or unsent DATA.
#[test]
fn shutdown_preserves_unacknowledged_and_unsent_gameplay() -> Result<()> {
    for pending in [false, true] {
        let (mut local, _) = reset_pair();
        let now = Instant::now();
        let ppi = PayloadProtocolIdentifier::Binary;
        local.cwnd = 2;
        for _ in 0..if pending { 3 } else { 2 } {
            local
                .stream(1)?
                .write_sctp(now, &Bytes::from_static(b"x"), ppi)?;
        }
        let data = transmitted_data(&local.gather_outbound(now).0);
        assert_eq!(data.len(), 2);
        let rwnd = local.rwnd;
        for (index, chunk) in data.iter().enumerate() {
            local.handle_shutdown(
                &ChunkShutdown {
                    cumulative_tsn_ack: chunk.tsn,
                },
                now,
            )?;
            assert_eq!(local.rwnd, rwnd, "SHUTDOWN has no window evidence");
            assert_eq!(local.pending_queue.len(), usize::from(pending));
            assert_eq!(local.inflight_queue.len(), 1 - index);
            assert_eq!(
                local.stream(1)?.buffered_amount()?,
                1 - index + usize::from(pending)
            );
            assert_eq!(
                local.state(),
                if index == 1 && !pending {
                    AssociationState::ShutdownAckSent
                } else {
                    AssociationState::ShutdownReceived
                }
            );
            if index == 0 {
                assert!(local.timers.get(Timer::T3RTX).is_some());
                assert!(
                    !local.inflight_queue.get(data[1].tsn).unwrap().acknowledged,
                    "cumulative credit must not become a selective ACK"
                );
            }
        }
    }
    Ok(())
}

// Issue #1143: SHUTDOWN must retire the same partial-reliability ACK state as SACK.
#[test]
fn shutdown_acknowledgment_retires_forward_tsn_and_reset_fence() -> Result<()> {
    let (mut local, _) = reset_pair();
    let now = Instant::now();
    local
        .stream(1)?
        .set_reliability_params(false, ReliabilityType::Rexmit, 0)?;
    local.stream(1)?.write_sctp(
        now,
        &Bytes::from_static(b"movement"),
        PayloadProtocolIdentifier::Binary,
    )?;
    local
        .open_stream(2, PayloadProtocolIdentifier::Binary)?
        .write_sctp(
            now,
            &Bytes::from_static(b"command"),
            PayloadProtocolIdentifier::Binary,
        )?;
    let data = transmitted_data(&local.gather_outbound(now).0);
    assert_eq!(data.len(), 2);
    let tsn = data[0].tsn;
    let cumulative_tsn_ack = data[1].tsn;
    // A previously retired stream retains its TSN fence until cumulative ACK.
    local.retired_reset_tsns.insert(9, tsn);
    let retry_at = local.timers.get(Timer::T3RTX).unwrap();
    local.handle_timeout(retry_at);
    local.gather_outbound(retry_at);
    assert_eq!(local.advanced_peer_tsn_ack_point, tsn);
    assert_eq!(local.fwd_tsn_stream_map.get(&1), Some(&0));
    assert_eq!(local.stream(1)?.buffered_amount()?, 0);
    while local.poll().is_some() {}

    local.handle_shutdown(&ChunkShutdown { cumulative_tsn_ack }, retry_at)?;
    assert_eq!(local.cumulative_tsn_ack_point, cumulative_tsn_ack);
    assert_eq!(local.advanced_peer_tsn_ack_point, cumulative_tsn_ack);
    assert!(local.retired_reset_tsns.is_empty());
    assert!(local.fwd_tsn_stream_map.is_empty());
    assert!(local.inflight_queue.is_empty());
    assert!(local.timers.get(Timer::T3RTX).is_none());
    let released: Vec<_> = std::iter::from_fn(|| local.poll())
        .filter_map(|event| {
            if let Event::Stream(StreamEvent::BufferedAmountReleased {
                id,
                data_channel_bytes,
                ..
            }) = event
            {
                Some((id, data_channel_bytes))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        released,
        vec![(2, 7)],
        "only the reliable command releases new bytes"
    );
    Ok(())
}

// Issue #1145: graceful close must drain both the admitted and transmitted tail.
#[test]
fn graceful_shutdown_drains_admitted_gameplay_in_wire_order() -> Result<()> {
    for simultaneous in [false, true] {
        for before_transmit in [false, true] {
            let (mut local, mut remote) = reset_pair();
            let now = Instant::now();
            local.ack_mode = AckMode::NoDelay;
            remote.ack_mode = AckMode::NoDelay;
            // A small window admits only part of the fragmented message.
            local.cwnd = local.max_payload_size;
            local.rwnd = local.max_payload_size;
            remote.max_receive_buffer_size = local.max_payload_size;
            let expected = vec![
                Bytes::from_static(b"first"),
                Bytes::from(vec![0xa5; local.max_payload_size as usize * 3 + 17]),
                Bytes::from_static(b"last"),
            ];
            for payload in &expected {
                local
                    .stream(1)?
                    .write_sctp(now, payload, PayloadProtocolIdentifier::Binary)?;
            }
            let first = if before_transmit {
                vec![]
            } else {
                local.gather_outbound(now).0
            };
            assert!(!local.pending_queue.is_empty());
            local.shutdown()?;
            assert_eq!(local.state(), AssociationState::ShutdownPending);
            assert!(
                local.poll_endpoint_event().is_none(),
                "shutdown initiation must keep packet routing"
            );
            assert!(
                local
                    .stream(1)?
                    .write_sctp(
                        now,
                        &Bytes::from_static(b"late"),
                        PayloadProtocolIdentifier::Binary
                    )
                    .is_err()
            );
            if simultaneous {
                remote.shutdown()?;
            }
            // Lose the first DATA packet, forcing a retry during graceful drain.
            drop(first);
            let mut received = vec![];
            let mut shutdown_seen = false;
            for step in 0..200 {
                let at = now + Duration::from_millis(step * 100);
                local.handle_timeout(at);
                remote.handle_timeout(at);
                for raw in local.gather_outbound(at).0 {
                    let packet = Packet::unmarshal(&raw)?;
                    if packet
                        .chunks
                        .iter()
                        .any(|c| c.as_any().is::<ChunkShutdown>())
                    {
                        shutdown_seen = true;
                        assert!(local.pending_queue.is_empty());
                        assert!(
                            local.inflight_queue.is_empty(),
                            "SHUTDOWN must wait for the cumulative tail ACK"
                        );
                        assert_eq!(received, expected);
                    }
                    remote.handle_inbound(packet, at)?;
                    if remote.streams.contains_key(&1) {
                        while let Some(message) = remote.stream(1)?.read_sctp()? {
                            received.push(message.to_payload(65536)?.freeze());
                        }
                    }
                }
                for raw in remote.gather_outbound(at).0 {
                    local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                if local.state() == AssociationState::Closed
                    && remote.state() == AssociationState::Closed
                {
                    break;
                }
            }
            assert_eq!(
                received, expected,
                "simultaneous={simultaneous}, before_transmit={before_transmit}"
            );
            assert_eq!(local.state(), AssociationState::Closed);
            assert_eq!(remote.state(), AssociationState::Closed);
            assert!(shutdown_seen || simultaneous);
            assert!(local.poll_timeout().is_none());
            assert!(remote.poll_timeout().is_none());
            assert!(local.poll_endpoint_event().is_some());
            assert!(local.poll_endpoint_event().is_none());
            assert!(remote.poll_endpoint_event().is_some());
            assert!(remote.poll_endpoint_event().is_none());
        }
    }
    Ok(())
}

#[test]
fn graceful_shutdown_without_data_retires_after_final_packet() -> Result<()> {
    let (mut local, mut remote) = reset_pair();
    let now = Instant::now();
    local.shutdown()?;
    assert!(local.poll_endpoint_event().is_none());
    for raw in local.gather_outbound(now).0 {
        remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    assert!(remote.poll_endpoint_event().is_none());
    for raw in remote.gather_outbound(now).0 {
        local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    assert!(
        local.poll_endpoint_event().is_none(),
        "the final packet has not been returned yet"
    );
    let final_packets = local.gather_outbound(now).0;
    assert!(
        final_packets
            .iter()
            .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
            .any(|c| c.as_any().is::<ChunkShutdownComplete>())
    );
    assert_eq!(local.state(), AssociationState::Closed);
    assert!(local.poll_endpoint_event().is_some());
    for raw in final_packets {
        remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    assert_eq!(remote.state(), AssociationState::Closed);
    assert!(remote.poll_endpoint_event().is_some());
    assert!(local.gather_outbound(now).0.is_empty());
    assert!(remote.gather_outbound(now).0.is_empty());
    Ok(())
}

// Issue #1145: a reader must reopen a zero window without waiting for a DATA retry.
#[test]
fn application_read_reopens_full_window_during_gameplay_and_shutdown() -> Result<()> {
    for (name, local_shutdown, remote_shutdown) in [
        ("gameplay", false, false),
        ("local shutdown", true, false),
        ("peer shutdown", false, true),
        ("simultaneous shutdown", true, true),
    ] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        local.ack_mode = AckMode::NoDelay;
        remote.ack_mode = AckMode::NoDelay;
        local.rwnd = 3;
        remote.max_receive_buffer_size = 3;
        let first = Bytes::from_static(b"old");
        let tail = [b"a".as_slice(), b"b", b"c"];
        local
            .stream(1)?
            .write_sctp(now, &first, PayloadProtocolIdentifier::Binary)?;
        for payload in tail {
            local.stream(1)?.write(now, payload)?;
        }
        if local_shutdown {
            local.shutdown()?;
        }
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        if remote_shutdown {
            remote.shutdown()?;
        }
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert_eq!(local.rwnd, 0);
        assert!(local.inflight_queue.is_empty());
        assert_eq!(local.pending_queue.len(), 3);
        assert_eq!(remote.get_my_receiver_window_credit(), 0);
        assert_eq!(
            remote
                .stream(1)?
                .read_sctp()?
                .unwrap()
                .to_payload(3)?
                .freeze(),
            first
        );
        assert_eq!(remote.get_my_receiver_window_credit(), 3);
        let updates = remote.gather_outbound(now).0;
        assert!(
            !updates.is_empty(),
            "{name}: the read must advertise reopened credit"
        );
        for raw in updates {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert_eq!(local.rwnd, 3);
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert!(
            local.pending_queue.is_empty(),
            "all admitted tail DATA must leave without a retry timer"
        );
        for payload in tail {
            assert_eq!(
                remote
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(3)?
                    .freeze()
                    .as_ref(),
                payload
            );
        }
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        if local_shutdown || remote_shutdown {
            for raw in local.gather_outbound(now).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            for raw in remote.gather_outbound(now).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            for raw in local.gather_outbound(now).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            assert_eq!(local.state(), AssociationState::Closed);
            assert_eq!(remote.state(), AssociationState::Closed);
        }
    }
    Ok(())
}

#[test]
fn shutdown_window_update_preserves_delayed_ack_and_empty_read_policy() -> Result<()> {
    for full in [false, true] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        remote.ack_mode = AckMode::Normal;
        remote.max_receive_buffer_size = if full { 1 } else { 2 };
        local.stream(1)?.write(now, b"x")?;
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert_eq!(remote.ack_state, AckState::Delay);
        let ack_at = remote.timers.get(Timer::Ack).unwrap();
        assert!(remote.stream(1)?.read_sctp()?.is_some());
        assert_eq!(
            remote.ack_state,
            if full {
                AckState::Immediate
            } else {
                AckState::Delay
            }
        );
        assert_eq!(
            remote.timers.get(Timer::Ack),
            if full { None } else { Some(ack_at) }
        );
        let updates = remote.gather_outbound(now).0;
        assert_eq!(!updates.is_empty(), full);
        assert!(remote.stream(1)?.read_sctp()?.is_none());
        assert!(
            remote.gather_outbound(now).0.is_empty(),
            "an empty read must not emit a window update"
        );
        if !full {
            assert_eq!(remote.timers.get(Timer::Ack), Some(ack_at));
            remote.handle_timeout(ack_at);
            assert!(!remote.gather_outbound(ack_at).0.is_empty());
        }
    }
    Ok(())
}

// Issue #1145: transport completion must not discard an unread accepted tail.
#[test]
fn graceful_shutdown_preserves_messages_until_application_reads() -> Result<()> {
    for (simultaneous, unread_tail) in [(false, 0_u64), (true, 0), (false, 1024), (true, 1024)] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        local.ack_mode = AckMode::NoDelay;
        remote.ack_mode = AckMode::NoDelay;
        let mut expected = vec![
            Bytes::from_static(b"one"),
            Bytes::from(vec![0x5a; 4096]),
            Bytes::from_static(b"tail"),
        ];
        expected.extend(
            (0..unread_tail).map(|sequence| Bytes::copy_from_slice(&sequence.to_be_bytes())),
        );
        for endpoint in [&mut local, &mut remote] {
            for payload in &expected {
                endpoint
                    .stream(1)?
                    .write_sctp(now, payload, PayloadProtocolIdentifier::Binary)?;
            }
        }
        local.shutdown()?;
        if simultaneous {
            remote.shutdown()?;
        }
        for step in 0..100 {
            let at = now + Duration::from_millis(step * 100);
            local.handle_timeout(at);
            remote.handle_timeout(at);
            for raw in local.gather_outbound(at).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
            for raw in remote.gather_outbound(at).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
            if local.state() == AssociationState::Closed
                && remote.state() == AssociationState::Closed
            {
                break;
            }
        }
        for endpoint in [&mut local, &mut remote] {
            assert_eq!(endpoint.state(), AssociationState::Closed);
            assert!(endpoint.poll_timeout().is_none());
            assert!(endpoint.poll_endpoint_event().is_none());
            assert!(
                !endpoint
                    .events
                    .iter()
                    .any(|event| matches!(event, Event::AssociationLost { id: 1, .. })),
                "stream close must follow the final read"
            );
            assert!(
                endpoint
                    .stream(1)?
                    .write_sctp(
                        now,
                        &Bytes::from_static(b"late"),
                        PayloadProtocolIdentifier::Binary
                    )
                    .is_err()
            );
            let mut received = vec![];
            for _ in &expected {
                assert!(endpoint.poll_endpoint_event().is_none());
                let message = endpoint.stream(1)?.read_sctp()?.unwrap();
                if message.chunks.len() == 1 {
                    assert_eq!(
                        message.chunks.capacity(),
                        1,
                        "unread gameplay must retain only its admitted fragment"
                    );
                }
                received.push(message.into_payload(65536)?.freeze());
            }
            assert_eq!(received, expected);
            assert!(endpoint.poll_endpoint_event().is_some());
            assert!(endpoint.poll_endpoint_event().is_none());
            assert!(endpoint.streams.is_empty());
            assert_eq!(
                endpoint
                    .events
                    .iter()
                    .filter(|event| matches!(event, Event::AssociationLost { id: 1, .. }))
                    .count(),
                1
            );
        }
    }
    Ok(())
}

#[test]
fn graceful_shutdown_retries_lost_shutdown_and_ack() -> Result<()> {
    for (lose_shutdown, lose_ack) in [(true, false), (false, true), (true, true)] {
        let (mut local, mut remote) = reset_pair();
        let mut now = Instant::now();
        local.shutdown()?;
        let mut packets = local.gather_outbound(now).0;
        if lose_shutdown {
            assert!(!packets.is_empty());
            now = local.timers.get(Timer::T2Shutdown).unwrap();
            local.handle_timeout(now);
            packets = local.gather_outbound(now).0;
        }
        assert!(local.poll_endpoint_event().is_none());
        for raw in packets {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        let mut packets = remote.gather_outbound(now).0;
        if lose_ack {
            assert!(!packets.is_empty());
            let remote_deadline = remote.timers.get(Timer::T2Shutdown).unwrap();
            now = remote_deadline.max(local.timers.get(Timer::T2Shutdown).unwrap());
            local.handle_timeout(now);
            let repeated = local.gather_outbound(now).0;
            assert!(!repeated.is_empty());
            for raw in repeated {
                remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            assert!(!remote.gather_outbound(now).0.is_empty());
            assert_eq!(remote.timers.get(Timer::T2Shutdown), Some(remote_deadline));
            remote.handle_timeout(now);
            packets = remote.gather_outbound(now).0;
        }
        assert!(remote.poll_endpoint_event().is_none());
        for raw in packets {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert!(local.poll_endpoint_event().is_none());
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        for endpoint in [&mut local, &mut remote] {
            assert_eq!(endpoint.state(), AssociationState::Closed);
            assert!(endpoint.poll_timeout().is_none());
            assert!(endpoint.poll_endpoint_event().is_some());
            assert!(endpoint.poll_endpoint_event().is_none());
        }
    }
    Ok(())
}

#[test]
fn graceful_shutdown_allows_stopping_an_unread_closed_tail() -> Result<()> {
    let (mut local, mut remote) = reset_pair();
    let now = Instant::now();
    remote.stream(1)?.write_sctp(
        now,
        &Bytes::from_static(b"unread"),
        PayloadProtocolIdentifier::Binary,
    )?;
    for raw in remote.gather_outbound(now).0 {
        local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    local.close(AssociationError::AssociationClosed)?;
    assert!(local.stream(1)?.is_readable());
    local.stream(1)?.stop(now)?;
    assert!(local.streams.is_empty());
    assert!(local.poll_timeout().is_none());
    assert!(local.pending_queue.is_empty());
    assert_eq!(
        local
            .events
            .iter()
            .filter(|event| matches!(event, Event::AssociationLost { id: 1, .. }))
            .count(),
        1
    );
    Ok(())
}

// Issue #1145: terminal close must retain DATA held across a stream reset.
#[test]
fn graceful_shutdown_preserves_accepted_reset_generation_tail() -> Result<()> {
    for disposition in [
        "read",
        "stop",
        "stop while draining",
        "abort",
        "transport close",
    ] {
        let (a, mut b) = reset_pair();
        let now = Instant::now();
        let old = ChunkPayloadData {
            tsn: b.peer_last_tsn.wrapping_add(1),
            stream_identifier: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"old tail"),
            ..Default::default()
        };
        b.handle_data(&old)?;
        b.send_reset_request(now, 1)?;
        let reset = reset_requests(&mut b, now).remove(0);
        b.handle_reconfig_param(
            &response(
                request(&reset).reconfig_request_sequence_number,
                ReconfigResult::SuccessPerformed,
            ),
            &mut vec![],
            now,
        )?;
        let peer_reset = Box::new(ParamOutgoingResetRequest {
            reconfig_request_sequence_number: a.my_next_rsn,
            reconfig_response_sequence_number: b.my_next_rsn.wrapping_sub(1),
            sender_last_tsn: old.tsn,
            stream_identifiers: vec![1],
        }) as Box<dyn Param>;
        b.handle_reconfig_param(&peer_reset, &mut vec![], now)?;
        b.handle_data(&ChunkPayloadData {
            tsn: old.tsn.wrapping_add(1),
            user_data: Bytes::from_static(b"saved next generation"),
            ..old
        })?;
        assert!(b.deferred_reset_reassembly.get(&1).unwrap().is_readable());
        if disposition == "stop while draining" {
            b.set_state(AssociationState::ShutdownPending);
            b.stream(1)?.stop(now)?;
            assert!(b.deferred_reset_reassembly.is_empty());
            assert_eq!(
                b.streams
                    .get(&1)
                    .unwrap()
                    .get_num_bytes_in_reassembly_queue(),
                21
            );
            assert!(
                b.stream(1)?.is_readable(),
                "the saved generation keeps its reader"
            );
        }
        b.close(AssociationError::AssociationClosed)?;
        b.endpoint_events.push_back(EndpointEventInner::Drained);
        assert!(b.poll_endpoint_event().is_none());
        if matches!(disposition, "abort" | "transport close") {
            for _ in 0..2 {
                if disposition == "abort" {
                    b.abort()?;
                } else {
                    b.close(AssociationError::ApplicationClosed)?;
                }
            }
            assert!(b.streams.is_empty());
            assert!(b.deferred_reset_reassembly.is_empty());
            assert!(b.deferred_reset_data.is_empty());
            assert_eq!(b.deferred_reset_bytes, 0);
            assert!(b.poll_endpoint_event().is_some());
            assert!(b.poll_endpoint_event().is_none());
            assert!(b.poll_timeout().is_none());
            assert_eq!(
                b.events
                    .iter()
                    .filter(|event| matches!(event, Event::AssociationLost { id: 1, .. }))
                    .count(),
                1
            );
            continue;
        }
        if disposition != "stop while draining" {
            let mut old_stream = b.stream(1)?;
            if disposition == "stop" {
                old_stream.stop(now)?;
            } else {
                assert_eq!(
                    old_stream.read_sctp()?.unwrap().to_payload(100)?.freeze(),
                    Bytes::from_static(b"old tail")
                );
            }
            assert!(
                old_stream.read_sctp()?.is_none(),
                "old handle cannot consume the saved generation"
            );
        }
        assert!(b.poll_endpoint_event().is_none());
        let mut next = b
            .accept_stream()
            .expect("saved generation is accepted for reads");
        assert!(!next.is_writable());
        assert_eq!(
            next.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"saved next generation")
        );
        assert!(next.read_sctp()?.is_none());
        assert!(b.poll_endpoint_event().is_some());
        assert!(b.poll_endpoint_event().is_none());
        assert!(b.streams.is_empty());
        assert!(b.deferred_reset_reassembly.is_empty());
        assert!(b.deferred_reset_data.is_empty());
        assert_eq!(b.deferred_reset_bytes, 0);
        assert!(b.poll_timeout().is_none());
    }
    Ok(())
}

// PR #1148: stopping reads before graceful transport closure must stay final.
#[test]
fn graceful_shutdown_does_not_revive_stopped_receivers() -> Result<()> {
    for finish_first in [false, true] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        remote.stream(1)?.write_sctp(
            now,
            &Bytes::from_static(b"stopped unread tail"),
            PayloadProtocolIdentifier::Binary,
        )?;
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert!(local.has_readable_data());
        if finish_first {
            local.stream(1)?.finish()?;
        }
        local.stream(1)?.stop(now)?;
        assert!(
            local
                .streams
                .get(&1)
                .unwrap()
                .reassembly_queue
                .is_readable()
        );
        assert!(!local.stream(1)?.is_readable());
        assert!(
            !local.has_readable_data(),
            "stopped queues cannot delay endpoint retirement"
        );
        assert!(
            local.has_complete_queued_data(),
            "stopped queues still block receive-buffer overflow"
        );
        local.max_receive_buffer_size = 1;
        let tail = local.peer_last_tsn;
        local.handle_data(&ChunkPayloadData {
            tsn: tail.wrapping_add(1),
            stream_identifier: 1,
            stream_sequence_number: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"must remain backpressured"),
            ..Default::default()
        })?;
        assert_eq!(
            local.peer_last_tsn, tail,
            "stopped complete queues cannot bypass a full receive window"
        );
        local.close(AssociationError::AssociationClosed)?;
        local.endpoint_events.push_back(EndpointEventInner::Drained);
        assert!(
            local.streams.is_empty(),
            "graceful close cannot revive stopped reads"
        );
        assert!(
            local.poll_endpoint_event().is_some(),
            "no unreadable queue may keep routing alive"
        );
        assert!(local.poll_endpoint_event().is_none());
        assert!(local.poll_timeout().is_none());
    }
    Ok(())
}

// Issue #1145: stopping reads during graceful drain must release receive pressure.
#[test]
fn graceful_shutdown_stop_drains_peer_tail_without_reopening_reads() -> Result<()> {
    for (peer_shutdown, stop_before_shutdown) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        local.ack_mode = AckMode::NoDelay;
        remote.ack_mode = AckMode::NoDelay;
        local.max_receive_buffer_size = 3;
        remote.rwnd = 3;
        remote.stream(1)?.write(now, b"old")?;
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert_eq!(local.get_my_receiver_window_credit(), 0);
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert!(remote.inflight_queue.is_empty());
        let expected = Bytes::from_static(b"local admitted tail");
        local
            .stream(1)?
            .write_sctp(now, &expected, PayloadProtocolIdentifier::Binary)?;
        if stop_before_shutdown {
            local.stream(1)?.stop(now)?;
            assert_eq!(
                local.get_my_receiver_window_credit(),
                0,
                "established reset pressure stays unchanged"
            );
        }
        if peer_shutdown {
            remote.shutdown()?;
            for raw in remote.gather_outbound(now).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            assert_eq!(local.state(), AssociationState::ShutdownReceived);
        } else {
            for payload in [b"tail".as_slice(), b"last"] {
                remote.stream(1)?.write(now, payload)?;
            }
            assert!(!remote.pending_queue.is_empty());
            local.shutdown()?;
            assert_eq!(local.state(), AssociationState::ShutdownPending);
        }
        local.stream(1)?.stop(now)?;
        local.stream(1)?.stop(now)?;
        assert!(!local.stream(1)?.is_readable());
        assert!(local.stream(1)?.read_sctp().is_err());
        assert_eq!(local.get_my_receiver_window_credit(), 3);
        let mut received = vec![];
        for _ in 0..20 {
            for raw in local.gather_outbound(now).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                if remote.streams.contains_key(&1) {
                    while let Some(message) = remote.stream(1)?.read_sctp()? {
                        received.push(message.to_payload(100)?.freeze());
                    }
                }
            }
            for raw in remote.gather_outbound(now).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            if local.state() == AssociationState::Closed
                && remote.state() == AssociationState::Closed
            {
                break;
            }
        }
        assert_eq!(received, vec![expected]);
        assert_eq!(
            local.peer_last_tsn,
            remote.my_next_tsn.wrapping_sub(1),
            "all peer DATA is received or deliberately discarded"
        );
        assert_eq!(
            remote.cumulative_tsn_ack_point,
            remote.my_next_tsn.wrapping_sub(1),
            "the peer tail is cumulatively acknowledged"
        );
        for endpoint in [&mut local, &mut remote] {
            assert_eq!(endpoint.state(), AssociationState::Closed);
            // Terminal closure does not need a reciprocal stream-reset marker.
            assert_eq!(endpoint.pending_queue.get_num_bytes(), 0);

            assert!(endpoint.inflight_queue.is_empty());
            assert!(endpoint.poll_timeout().is_none());
            assert!(endpoint.poll_endpoint_event().is_some());
            assert!(endpoint.poll_endpoint_event().is_none());
        }
    }
    Ok(())
}

#[test]
fn graceful_shutdown_stop_accepts_every_draining_state() -> Result<()> {
    for state in [
        AssociationState::ShutdownPending,
        AssociationState::ShutdownReceived,
        AssociationState::ShutdownSent,
        AssociationState::ShutdownAckSent,
    ] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        remote.stream(1)?.write(now, b"unread")?;
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        local.set_state(state);
        local.stream(1)?.stop(now)?;
        local.stream(1)?.stop(now)?;
        assert!(!local.stream(1)?.is_readable(), "{state:?}");
        assert!(local.stream(1)?.read_sctp().is_err(), "{state:?}");
        assert!(!local.has_readable_data(), "{state:?}");
        assert!(
            local.pending_queue.is_empty(),
            "stop must not start a new reset in {state:?}"
        );
        assert_eq!(
            local
                .streams
                .get(&1)
                .unwrap()
                .get_num_bytes_in_reassembly_queue(),
            0
        );
    }
    Ok(())
}

// Issue #1158: reordered gameplay at TSN wrap must report the exact selective ACK.
#[test]
fn shutdown_reordered_wrapped_gameplay_preserves_gap_ack_and_delivery() -> Result<()> {
    for first_tsn in [1u32, u32::MAX - 1, u32::MAX] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        local.my_next_tsn = first_tsn;
        local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
        remote.peer_last_tsn = first_tsn.wrapping_sub(1);
        remote.ack_mode = AckMode::NoDelay;
        let expected = [b"first".as_slice(), b"second", b"third"];
        for payload in expected {
            local.stream(1)?.write(now, payload)?;
        }
        local.shutdown()?;
        let sent = transmitted_data(&local.gather_outbound(now).0);
        assert_eq!(sent.len(), 3);
        for data in &sent[1..] {
            remote.handle_inbound(local.create_packet(vec![Box::new(data.clone())]), now)?;
        }
        let sacks = remote.gather_outbound(now).0;
        let sack = sacks
            .iter()
            .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
            .find_map(|chunk| {
                chunk
                    .as_any()
                    .downcast_ref::<ChunkSelectiveAck>()
                    .map(|sack| {
                        sack.gap_ack_blocks
                            .iter()
                            .map(|gap| (gap.start, gap.end))
                            .collect::<Vec<_>>()
                    })
            })
            .expect("out-of-order DATA emits a SACK");
        assert_eq!(sack.len(), 1, "first_tsn={first_tsn}");
        assert_eq!(sack[0], (2, 3));
        for raw in sacks {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert!(
            local
                .inflight_queue
                .get(first_tsn)
                .unwrap()
                .is_outstanding()
        );
        for data in &sent[1..] {
            assert!(!local.inflight_queue.get(data.tsn).unwrap().is_outstanding());
        }
        assert_eq!(local.state(), AssociationState::ShutdownPending);
        let retry_at = local
            .poll_timeout()
            .expect("missing DATA keeps recovery armed");
        local.handle_timeout(retry_at);
        let retry = transmitted_data(&local.gather_outbound(retry_at).0);
        assert_eq!(
            retry.len(),
            1,
            "selectively acknowledged tails must not retry"
        );
        assert_eq!(retry[0].tsn, first_tsn);
        assert_eq!(retry[0].user_data.as_ref(), expected[0]);
        remote.handle_inbound(
            local.create_packet(vec![Box::new(retry[0].clone())]),
            retry_at,
        )?;
        for payload in expected {
            assert_eq!(
                remote
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(100)?
                    .as_ref(),
                payload
            );
        }
        for _ in 0..3 {
            for raw in remote.gather_outbound(retry_at).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, retry_at)?;
            }
            for raw in local.gather_outbound(retry_at).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, retry_at)?;
            }
        }
        assert_eq!(local.state(), AssociationState::Closed);
        assert_eq!(remote.state(), AssociationState::Closed);
        assert!(local.poll_timeout().is_none());
        assert!(remote.poll_timeout().is_none());
    }
    Ok(())
}

#[test]
fn shutdown_selective_ack_stays_within_mtu_after_reordered_gameplay() -> Result<()> {
    for first_tsn in [1_u32, u32::MAX - 400] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        local.my_next_tsn = first_tsn;
        local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
        remote.peer_last_tsn = first_tsn.wrapping_sub(1);
        remote.ack_mode = AckMode::NoDelay;
        local.cwnd = 65536;
        let expected: Vec<_> = (0_u16..800).map(|n| n.to_be_bytes()).collect();
        for payload in &expected {
            local.stream(1)?.write(now, payload)?;
        }
        local.shutdown()?;
        let sent = transmitted_data(&local.gather_outbound(now).0);
        assert_eq!(sent.len(), expected.len());
        for data in sent.iter().skip(1).step_by(2) {
            remote.handle_inbound(local.create_packet(vec![Box::new(data.clone())]), now)?;
            remote.handle_inbound(local.create_packet(vec![Box::new(data.clone())]), now)?;
        }
        let receive_credit = remote.get_my_receiver_window_credit();
        let packets = remote.gather_outbound(now).0;
        assert!(!packets.is_empty());
        for raw in packets {
            assert!(
                raw.len() <= remote.mtu as usize,
                "SACK is {} bytes, MTU is {}",
                raw.len(),
                remote.mtu
            );
            let packet = Packet::unmarshal(&raw)?;
            let sack = packet
                .chunks
                .iter()
                .find_map(|c| c.as_any().downcast_ref::<ChunkSelectiveAck>())
                .unwrap();
            assert_eq!(sack.cumulative_tsn_ack, first_tsn.wrapping_sub(1));
            assert_eq!(sack.advertised_receiver_window_credit, receive_credit);
            assert_eq!(sack.gap_ack_blocks.len(), 350);
            assert_eq!(raw.len(), remote.mtu as usize);
            for (index, gap) in sack.gap_ack_blocks.iter().enumerate() {
                assert_eq!(
                    (gap.start, gap.end),
                    ((index as u16 + 1) * 2, (index as u16 + 1) * 2)
                );
            }
            local.handle_inbound(packet, now)?;
        }
        // Truncation must retain DATA, not remove the unreported receive tail.
        for data in sent.iter().step_by(2) {
            remote.handle_inbound(local.create_packet(vec![Box::new(data.clone())]), now)?;
        }
        for payload in &expected {
            assert_eq!(
                remote
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(2)?
                    .as_ref(),
                payload
            );
        }
        for _ in 0..4 {
            for raw in remote.gather_outbound(now).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            for raw in local.gather_outbound(now).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
        }
        assert_eq!(local.state(), AssociationState::Closed);
        assert_eq!(remote.state(), AssociationState::Closed);
    }
    Ok(())
}

#[test]
fn shutdown_selective_ack_budget_prioritizes_gaps_and_fits_duplicate_reports() -> Result<()> {
    for (mtu, gaps, duplicates) in [
        (28, 0, 0),
        (40, 1, 2),
        (41, 1, 2),
        (44, 4, 0),
        (48, 4, 1),
        (65548, 16376, 3),
    ] {
        let mut a = timed_test_association();
        a.mtu = mtu;
        a.peer_last_tsn = 0;
        for index in 1..=gaps {
            let data = ChunkPayloadData {
                tsn: index * 2,
                ..Default::default()
            };
            assert!(a.payload_queue.push(data, 0));
        }
        // Queue-level duplicates exercise the optional SACK report budget.
        for _ in 0..20 {
            assert!(!a.payload_queue.push(ChunkPayloadData::default(), 0));
        }
        let sack = a.create_selective_ack_chunk();
        assert_eq!(sack.gap_ack_blocks.len(), gaps as usize);
        assert_eq!(sack.duplicate_tsn, vec![0; duplicates]);
        let raw = a.marshal_control_chunk(&sack)?;
        assert!(raw.len() <= mtu as usize);
        let decoded = Packet::unmarshal(&raw)?;
        let decoded = decoded.chunks[0]
            .as_any()
            .downcast_ref::<ChunkSelectiveAck>()
            .unwrap();
        assert_eq!(decoded.gap_ack_blocks.len(), gaps as usize);
        assert_eq!(decoded.duplicate_tsn, sack.duplicate_tsn);
        assert!(a.payload_queue.pop_duplicates().is_empty());
    }
    Ok(())
}

// Issue #1165: lost SACKs must not delay or hide duplicate-only DATA reports.
#[test]
fn shutdown_duplicate_data_immediately_reports_exact_tsns_without_double_credit() -> Result<()> {
    for first_tsn in [1u32, u32::MAX] {
        for selective in [false, true] {
            for graceful in [false, true] {
                let (mut local, mut remote) = reset_pair();
                let now = Instant::now();
                local.my_next_tsn = first_tsn;
                local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
                local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
                remote.peer_last_tsn = first_tsn.wrapping_sub(1);
                remote.ack_mode = AckMode::Normal;
                let payloads = [b"first".as_slice(), b"second".as_slice()];
                for payload in payloads {
                    local.stream(1)?.write(now, payload)?;
                }
                let sent = transmitted_data(&local.gather_outbound(now).0);
                assert_eq!(sent.len(), 2);
                assert!(!sent[0].immediate_sack && !sent[1].immediate_sack);
                if graceful {
                    local.shutdown()?;
                }
                let duplicate = &sent[usize::from(selective)];
                remote
                    .handle_inbound(local.create_packet(vec![Box::new(duplicate.clone())]), now)?;
                let ack_at = if selective {
                    now
                } else {
                    assert!(remote.gather_outbound(now).0.is_empty());
                    let at = remote
                        .timers
                        .get(Timer::Ack)
                        .expect("ordinary DATA delays ACK");
                    remote.handle_timeout(at);
                    at
                };
                let lost = remote.gather_outbound(ack_at).0;
                assert!(lost.iter().any(|raw| {
                    Packet::unmarshal(raw)
                        .unwrap()
                        .chunks
                        .iter()
                        .any(|chunk| chunk.as_any().is::<ChunkSelectiveAck>())
                }));
                let receive_credit = remote.get_my_receiver_window_credit();
                let retained_bytes = remote
                    .streams
                    .get(&1)
                    .unwrap()
                    .get_num_bytes_in_reassembly_queue();
                for attempt in 0..2 {
                    remote.handle_inbound(
                        local.create_packet(vec![Box::new(duplicate.clone())]),
                        ack_at,
                    )?;
                    let replies = remote.gather_outbound(ack_at).0;
                    let mut reports = 0;
                    for raw in replies {
                        assert!(raw.len() <= remote.mtu as usize);
                        let packet = Packet::unmarshal(&raw)?;
                        for chunk in &packet.chunks {
                            if let Some(sack) = chunk.as_any().downcast_ref::<ChunkSelectiveAck>() {
                                reports += 1;
                                assert_eq!(
                                    sack.duplicate_tsn,
                                    vec![duplicate.tsn],
                                    "first_tsn={first_tsn}, selective={selective}, graceful={graceful}, attempt={attempt}"
                                );
                                assert_eq!(
                                    sack.cumulative_tsn_ack,
                                    if selective {
                                        first_tsn.wrapping_sub(1)
                                    } else {
                                        first_tsn
                                    }
                                );
                                assert_eq!(sack.gap_ack_blocks.len(), usize::from(selective));
                                if selective {
                                    assert_eq!(
                                        (sack.gap_ack_blocks[0].start, sack.gap_ack_blocks[0].end),
                                        (2, 2)
                                    );
                                }
                            }
                        }
                        local.handle_inbound(packet, ack_at)?;
                    }
                    assert_eq!(
                        reports, 1,
                        "duplicate-only DATA must emit a SACK without advancing time"
                    );
                    assert!(remote.timers.get(Timer::Ack).is_none());
                    assert_eq!(remote.get_my_receiver_window_credit(), receive_credit);
                    assert_eq!(
                        remote
                            .streams
                            .get(&1)
                            .unwrap()
                            .get_num_bytes_in_reassembly_queue(),
                        retained_bytes
                    );
                    assert_eq!(
                        local.stream(1)?.buffered_amount()?,
                        payloads[usize::from(!selective)].len()
                    );
                }
                let released: usize = std::iter::from_fn(|| local.poll())
                    .filter_map(|event| {
                        if let Event::Stream(StreamEvent::BufferedAmountReleased {
                            data_channel_bytes,
                            ..
                        }) = event
                        {
                            Some(data_channel_bytes)
                        } else {
                            None
                        }
                    })
                    .sum();
                assert_eq!(released, duplicate.user_data.len());
                let missing = &sent[usize::from(!selective)];
                remote
                    .handle_inbound(local.create_packet(vec![Box::new(missing.clone())]), ack_at)?;
                if let Some(at) = remote.timers.get(Timer::Ack) {
                    remote.handle_timeout(at);
                }
                for raw in remote.gather_outbound(ack_at).0 {
                    local.handle_inbound(Packet::unmarshal(&raw)?, ack_at)?;
                }
                for payload in payloads {
                    assert_eq!(
                        remote
                            .stream(1)?
                            .read_sctp()?
                            .unwrap()
                            .to_payload(100)?
                            .as_ref(),
                        payload
                    );
                }
                assert!(remote.stream(1)?.read_sctp()?.is_none());
                assert_eq!(local.stream(1)?.buffered_amount()?, 0);
                if graceful {
                    for _ in 0..3 {
                        for raw in local.gather_outbound(ack_at).0 {
                            remote.handle_inbound(Packet::unmarshal(&raw)?, ack_at)?;
                        }
                        for raw in remote.gather_outbound(ack_at).0 {
                            local.handle_inbound(Packet::unmarshal(&raw)?, ack_at)?;
                        }
                    }
                    assert_eq!(local.state(), AssociationState::Closed);
                    assert_eq!(remote.state(), AssociationState::Closed);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn stream_reset_duplicate_data_reports_saved_generation_without_double_delivery() -> Result<()> {
    for first_tsn in [1u32, u32::MAX] {
        for selective in [false, true] {
            let (mut local, mut remote) = reset_pair();
            let now = Instant::now();
            local.my_next_tsn = first_tsn;
            local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            remote.peer_last_tsn = first_tsn.wrapping_sub(1);
            remote.ack_mode = AckMode::Normal;
            local.send_reset_request(now, 1)?;
            let outgoing = reset_requests(&mut local, now).remove(0);
            for packet in remote.handle_reconfig(&outgoing, now)? {
                for chunk in packet.chunks {
                    if let Some(chunk) = chunk.as_any().downcast_ref::<ChunkReconfig>() {
                        local.handle_reconfig(chunk, now)?;
                    }
                }
            }
            let reciprocal = reset_requests(&mut remote, now).remove(0);
            let final_ack = local.handle_reconfig(&reciprocal, now)?;
            local.open_stream(1, PayloadProtocolIdentifier::Binary)?;
            let payloads = [b"new first".as_slice(), b"new second".as_slice()];
            for payload in payloads {
                local.stream(1)?.write(now, payload)?;
            }
            let sent = transmitted_data(&local.gather_outbound(now).0);
            assert_eq!(sent.len(), 2);
            let duplicate = &sent[usize::from(selective)];
            remote.handle_inbound(local.create_packet(vec![Box::new(duplicate.clone())]), now)?;
            assert!(!remote.gather_outbound(now).0.is_empty());
            let receive_credit = remote.get_my_receiver_window_credit();
            for _ in 0..2 {
                remote
                    .handle_inbound(local.create_packet(vec![Box::new(duplicate.clone())]), now)?;
                let replies = remote.gather_outbound(now).0;
                let mut reports = 0;
                for raw in replies {
                    assert!(raw.len() <= remote.mtu as usize);
                    let packet = Packet::unmarshal(&raw)?;
                    for chunk in &packet.chunks {
                        if let Some(sack) = chunk.as_any().downcast_ref::<ChunkSelectiveAck>() {
                            reports += 1;
                            assert_eq!(
                                sack.duplicate_tsn,
                                vec![duplicate.tsn],
                                "first_tsn={first_tsn}, selective={selective}"
                            );
                        }
                    }
                    local.handle_inbound(packet, now)?;
                }
                assert_eq!(reports, 1);
                assert_eq!(remote.deferred_reset_data.len(), 1);
                assert_eq!(remote.get_my_receiver_window_credit(), receive_credit);
                assert!(
                    !remote
                        .streams
                        .get(&1)
                        .unwrap()
                        .reassembly_queue
                        .is_readable()
                );
            }
            for packet in final_ack {
                for chunk in packet.chunks {
                    if let Some(chunk) = chunk.as_any().downcast_ref::<ChunkReconfig>() {
                        remote.handle_reconfig(chunk, now)?;
                    }
                }
            }
            assert!(remote.deferred_reset_data.is_empty());
            let missing = &sent[usize::from(!selective)];
            remote.handle_inbound(local.create_packet(vec![Box::new(missing.clone())]), now)?;
            for payload in payloads {
                assert_eq!(
                    remote
                        .stream(1)?
                        .read_sctp()?
                        .unwrap()
                        .to_payload(100)?
                        .as_ref(),
                    payload
                );
            }
            assert!(remote.stream(1)?.read_sctp()?.is_none());
        }
    }
    Ok(())
}

#[test]
fn shutdown_duplicate_data_packet_keeps_sack_within_mtu_budget() -> Result<()> {
    for (mtu, selective, expected_duplicates) in [
        (28, false, 0),
        (40, false, 3),
        (40, true, 2),
        (41, true, 2),
        (48, true, 4),
    ] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        remote.mtu = mtu;
        for payload in [b"one".as_slice(), b"two".as_slice()] {
            local.stream(1)?.write(now, payload)?;
        }
        let sent = transmitted_data(&local.gather_outbound(now).0);
        let duplicate = &sent[usize::from(selective)];
        remote.handle_inbound(local.create_packet(vec![Box::new(duplicate.clone())]), now)?;
        if let Some(at) = remote.timers.get(Timer::Ack) {
            remote.handle_timeout(at);
        }
        remote.gather_outbound(now);
        let chunks: Vec<Box<dyn Chunk>> = (0..20)
            .map(|_| Box::new(duplicate.clone()) as Box<dyn Chunk>)
            .collect();
        remote.handle_inbound(local.create_packet(chunks), now)?;
        let replies = remote.gather_outbound(now).0;
        assert_eq!(replies.len(), 1);
        assert!(replies[0].len() <= mtu as usize);
        let packet = Packet::unmarshal(&replies[0])?;
        let sack = packet.chunks[0]
            .as_any()
            .downcast_ref::<ChunkSelectiveAck>()
            .unwrap();
        assert_eq!(sack.duplicate_tsn, vec![duplicate.tsn; expected_duplicates]);
        assert_eq!(sack.gap_ack_blocks.len(), usize::from(selective));
        assert!(remote.payload_queue.pop_duplicates().is_empty());
    }
    Ok(())
}

#[test]
fn shutdown_selective_ack_generation_stops_at_packet_budget() {
    for cumulative in [0u32, u32::MAX - 1000] {
        for budget in [0usize, 1, 16, 128] {
            let mut association = timed_test_association();
            association.peer_last_tsn = cumulative;
            association.mtu = 28 + 4 * budget as u32;
            for offset in (2..=4096u32).step_by(2) {
                association.payload_queue.push(
                    ChunkPayloadData {
                        tsn: cumulative.wrapping_add(offset),
                        user_data: Bytes::from_static(b"retained"),
                        ..Default::default()
                    },
                    cumulative,
                );
            }
            association.payload_queue.gap_scan_steps.set(0);
            let sack = association.create_selective_ack_chunk();
            assert_eq!(sack.gap_ack_blocks.len(), budget);
            for (index, block) in sack.gap_ack_blocks.iter().enumerate() {
                assert_eq!(usize::from(block.start), 2 * (index + 1));
                assert_eq!(block.start, block.end);
            }
            let steps = association.payload_queue.gap_scan_steps.get();
            assert!(
                steps <= budget + usize::from(budget > 0),
                "budget {budget}: scanned {steps} chunks"
            );
            assert_eq!(association.payload_queue.len(), 2048);
            assert_eq!(association.payload_queue.get_num_bytes(), 2048 * 8);
        }
    }
}

#[test]
fn reassembly_mixed_completion_order_survives_reordering_and_shutdown() -> Result<()> {
    for first_tsn in [100u32, u32::MAX] {
        for order in [[0, 1, 2], [0, 2, 1]] {
            let (mut local, mut remote) = reset_pair();
            let now = Instant::now();
            local.my_next_tsn = first_tsn;
            local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            remote.peer_last_tsn = first_tsn.wrapping_sub(1);
            local.streams.get_mut(&1).unwrap().max_payload_size = 4;
            local.ack_mode = AckMode::NoDelay;
            remote.ack_mode = AckMode::NoDelay;
            local.stream(1)?.write(now, b"ordered!")?;
            local
                .stream(1)?
                .set_reliability_params(true, ReliabilityType::Reliable, 0)?;
            local.stream(1)?.write(now, b"fast")?;
            local.shutdown()?;
            let mut data = transmitted_data(&local.gather_outbound(now).0);
            assert_eq!(data.len(), 3);
            data.sort_by_key(|chunk| (chunk.unordered, !chunk.beginning_fragment));
            for index in order {
                remote.handle_inbound(
                    local.create_packet(vec![Box::new(data[index].clone())]),
                    now,
                )?;
            }
            let expected: [&[u8]; 2] = if order[1] == 1 {
                [b"ordered!", b"fast"]
            } else {
                [b"fast", b"ordered!"]
            };
            for payload in expected {
                assert_eq!(
                    remote
                        .stream(1)?
                        .read_sctp()?
                        .unwrap()
                        .to_payload(8)?
                        .as_ref(),
                    payload
                );
            }
            assert_eq!(
                remote
                    .streams
                    .get(&1)
                    .unwrap()
                    .get_num_bytes_in_reassembly_queue(),
                0
            );
            for _ in 0..4 {
                for raw in remote.gather_outbound(now).0 {
                    local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                }
                for raw in local.gather_outbound(now).0 {
                    remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                }
            }
            assert_eq!(local.state(), AssociationState::Closed);
            assert_eq!(remote.state(), AssociationState::Closed);
            assert!(local.inflight_queue.is_empty());
            assert!(local.pending_queue.is_empty());
        }
    }
    Ok(())
}

#[test]
fn shutdown_stream_admission_preserves_prehandshake_setup() -> Result<()> {
    for state in [
        AssociationState::Closed,
        AssociationState::CookieWait,
        AssociationState::CookieEchoed,
        AssociationState::Established,
    ] {
        let mut endpoint = timed_test_association();
        endpoint.set_state(state);
        assert!(!endpoint.retired);
        endpoint.open_stream(7, PayloadProtocolIdentifier::Binary)?;
        assert!(endpoint.stream(7)?.is_writable(), "{state}");
        assert!(matches!(
            endpoint.open_stream(7, PayloadProtocolIdentifier::Binary),
            Err(Error::ErrStreamAlreadyExist)
        ));
    }
    Ok(())
}

#[test]
fn graceful_shutdown_stream_readiness_matches_write_rejection() -> Result<()> {
    fn assert_write_closed(endpoint: &mut Association, state: AssociationState) -> Result<()> {
        assert_eq!(endpoint.state(), state);
        let stream_count = endpoint.streams.len();
        let next_generation = endpoint.next_stream_generation;
        assert!(
            matches!(
                endpoint.open_stream(7, PayloadProtocolIdentifier::Binary),
                Err(Error::ErrStreamCreateFailed)
            ),
            "{state}: shutdown cannot admit a new local stream"
        );
        assert_eq!(endpoint.streams.len(), stream_count);
        assert_eq!(endpoint.next_stream_generation, next_generation);
        let mut stream = endpoint.stream(1)?;
        assert!(
            !stream.is_writable(),
            "{state}: shutdown cannot accept writes"
        );
        let expected = Bytes::from_static(b"must stay with caller");
        let mut chunks = [expected.clone()];
        assert_eq!(
            stream.write_chunks(Instant::now(), &mut chunks),
            Err(Error::ErrStreamClosed)
        );
        assert_eq!(chunks, [expected], "{state}: refusal consumed caller data");
        assert_eq!(
            stream.write(Instant::now(), &[]),
            Err(Error::ErrStreamClosed)
        );
        assert_eq!(
            stream.write_sctp(
                Instant::now(),
                &Bytes::new(),
                PayloadProtocolIdentifier::Binary
            ),
            Err(Error::ErrStreamClosed)
        );
        assert_eq!(
            stream.write_chunks(Instant::now(), &mut [Bytes::new()]),
            Err(Error::ErrStreamClosed)
        );
        Ok(())
    }

    let (mut local, mut remote) = reset_pair();
    let now = Instant::now();
    local.ack_mode = AckMode::NoDelay;
    remote.ack_mode = AckMode::NoDelay;
    let local_tail = Bytes::from_static(b"local admitted gameplay");
    let remote_tail = Bytes::from_static(b"remote admitted gameplay");
    remote.open_stream(3, PayloadProtocolIdentifier::Binary)?;
    for (endpoint, id, payload) in [(&mut local, 1, &local_tail), (&mut remote, 3, &remote_tail)] {
        assert!(endpoint.stream(id)?.is_writable());
        endpoint
            .stream(id)?
            .write_sctp(now, payload, PayloadProtocolIdentifier::Binary)?;
    }
    local.shutdown()?;
    assert_write_closed(&mut local, AssociationState::ShutdownPending)?;
    for raw in local.gather_outbound(now).0 {
        remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    // Keep the remote admitted tail pending until it receives SHUTDOWN.
    for raw in remote.gather_outbound_sack_packets(vec![]) {
        local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    assert_write_closed(&mut local, AssociationState::ShutdownSent)?;
    for raw in local.gather_outbound(now).0 {
        remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    assert_write_closed(&mut remote, AssociationState::ShutdownReceived)?;
    for raw in remote.gather_outbound(now).0 {
        local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    for raw in local.gather_outbound(now).0 {
        remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    assert_write_closed(&mut remote, AssociationState::ShutdownAckSent)?;
    for _ in 0..2 {
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
    }
    for (endpoint, id, expected) in [(&mut local, 3, remote_tail), (&mut remote, 1, local_tail)] {
        assert_eq!(endpoint.state(), AssociationState::Closed);
        assert!(!endpoint.stream(id)?.is_writable());
        assert!(matches!(
            endpoint.open_stream(7, PayloadProtocolIdentifier::Binary),
            Err(Error::ErrStreamCreateFailed)
        ));
        assert_eq!(
            endpoint
                .stream(id)?
                .read_sctp()?
                .unwrap()
                .to_payload(100)?
                .freeze(),
            expected
        );
        assert!(endpoint.poll_endpoint_event().is_some());
    }
    Ok(())
}

#[test]
fn graceful_shutdown_preserves_unread_gameplay_across_full_stream_sequence_wrap() -> Result<()> {
    let (mut local, mut remote) = reset_pair();
    let now = Instant::now();
    let message_count = usize::from(u16::MAX) + 5;
    remote.max_receive_buffer_size = u32::try_from(message_count + 16).unwrap();
    remote.ack_mode = AckMode::NoDelay;
    for index in 0..message_count {
        let payload = Bytes::from(vec![u8::try_from(index % 251).unwrap()]);
        assert_eq!(
            local
                .stream(1)?
                .write_sctp(now, &payload, PayloadProtocolIdentifier::Binary)?,
            1
        );
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        assert!(
            local.inflight_queue.is_empty(),
            "message {index} was not acknowledged"
        );
        assert_eq!(
            remote
                .streams
                .get(&1)
                .unwrap()
                .get_num_bytes_in_reassembly_queue(),
            index + 1,
            "transport acknowledged message {index} without retaining its unread payload"
        );
    }
    local.shutdown()?;
    for _ in 0..2 {
        for raw in local.gather_outbound(now).0 {
            remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        for raw in remote.gather_outbound(now).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
    }
    assert_eq!(local.state(), AssociationState::Closed);
    assert_eq!(remote.state(), AssociationState::Closed);
    assert!(remote.poll_endpoint_event().is_none());
    for index in 0..message_count {
        let actual = remote
            .stream(1)?
            .read_sctp()?
            .expect("every acknowledged message stays readable");
        assert_eq!(
            actual.to_payload(1)?.freeze().as_ref(),
            &[u8::try_from(index % 251).unwrap()],
            "message {index}"
        );
    }
    assert!(remote.streams.is_empty());
    assert!(remote.poll_endpoint_event().is_some());
    assert!(remote.poll_endpoint_event().is_none());
    Ok(())
}

// Issue #1187: skip absent TSNs without discarding readable gameplay.
#[test]
fn shutdown_forward_tsn_work_tracks_retained_data_not_abandoned_distance() -> Result<()> {
    for base in [100, u32::MAX - 2048] {
        for shutdown in [false, true] {
            let (mut local, _) = reset_pair();
            local.peer_last_tsn = base;
            local.use_forward_tsn = true;
            for distance in [2_u32, 4096, 4098] {
                local.handle_data(&ChunkPayloadData {
                    tsn: base.wrapping_add(distance),
                    stream_identifier: 1,
                    unordered: true,
                    beginning_fragment: true,
                    ending_fragment: true,
                    payload_type: PayloadProtocolIdentifier::Binary,
                    user_data: Bytes::copy_from_slice(&distance.to_be_bytes()),
                    ..Default::default()
                })?;
            }
            if shutdown {
                local.shutdown()?;
            }
            local.payload_queue.pop_attempts = 0;
            local.handle_forward_tsn(&ChunkForwardTsn {
                new_cumulative_tsn: base.wrapping_add(4096),
                streams: vec![],
            })?;
            assert_eq!(local.peer_last_tsn, base.wrapping_add(4096));
            assert_eq!(local.payload_queue.len(), 1);
            assert_eq!(local.payload_queue.get_num_bytes(), 0);
            assert!(
                local.payload_queue.pop_attempts <= 3,
                "base={base}, shutdown={shutdown}: {} removal attempts for two retained chunks",
                local.payload_queue.pop_attempts
            );
            local.handle_data(&ChunkPayloadData {
                tsn: base.wrapping_add(4097),
                stream_identifier: 1,
                unordered: true,
                beginning_fragment: true,
                ending_fragment: true,
                payload_type: PayloadProtocolIdentifier::Binary,
                user_data: Bytes::copy_from_slice(&4097_u32.to_be_bytes()),
                ..Default::default()
            })?;
            assert_eq!(local.peer_last_tsn, base.wrapping_add(4098));
            assert!(local.payload_queue.is_empty());
            for distance in [2_u32, 4096, 4098, 4097] {
                assert_eq!(
                    local
                        .stream(1)?
                        .read_sctp()?
                        .unwrap()
                        .to_payload(4)?
                        .freeze(),
                    Bytes::copy_from_slice(&distance.to_be_bytes())
                );
            }
            assert_eq!(
                local.get_my_receiver_window_credit(),
                local.max_receive_buffer_size
            );
            assert!(local.stream(1)?.read_sctp()?.is_none());
        }
    }
    Ok(())
}

#[test]
fn empty_stream_writes_preserve_following_gameplay_and_shutdown() -> Result<()> {
    for source in ["slice", "bytes", "chunks"] {
        for first_ssn in [0, u16::MAX] {
            for shutdown in [false, true] {
                let (mut local, mut remote) = reset_pair();
                let now = Instant::now();
                local.ack_mode = AckMode::NoDelay;
                remote.ack_mode = AckMode::NoDelay;
                local.streams.get_mut(&1).unwrap().sequence_number = first_ssn;
                remote
                    .streams
                    .get_mut(&1)
                    .unwrap()
                    .reassembly_queue
                    .next_ssn = first_ssn;
                let next_message_id = local.next_message_id;
                for _ in 0..3 {
                    let mut stream = local.stream(1)?;
                    let written = match source {
                        "slice" => stream.write(now, &[])?,
                        "bytes" => stream.write_sctp(
                            now,
                            &Bytes::new(),
                            PayloadProtocolIdentifier::Binary,
                        )?,
                        "chunks" => stream.write_chunks(now, &mut [Bytes::new(), Bytes::new()])?,
                        _ => unreachable!(),
                    };
                    assert_eq!(written, 0);
                }
                assert!(local.pending_queue.is_empty());
                assert!(local.inflight_queue.is_empty());
                assert!(local.gather_outbound(now).0.is_empty());
                assert_eq!(local.next_message_id, next_message_id);
                assert_eq!(local.streams.get(&1).unwrap().sequence_number, first_ssn);
                let expected = [
                    Bytes::from_static(b"first gameplay"),
                    Bytes::from_static(b"second gameplay"),
                ];
                for payload in &expected {
                    local
                        .stream(1)?
                        .write_sctp(now, payload, PayloadProtocolIdentifier::Binary)?;
                }
                if shutdown {
                    local.shutdown()?;
                }
                for _ in 0..4 {
                    for raw in local.gather_outbound(now).0 {
                        remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                    }
                    for raw in remote.gather_outbound(now).0 {
                        local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                    }
                }
                for payload in expected {
                    let actual = remote.stream(1)?.read_sctp()?.unwrap_or_else(|| {
                        panic!("source={source}, first_ssn={first_ssn}, shutdown={shutdown}: empty writes blocked gameplay")
                    });
                    assert_eq!(actual.to_payload(100)?.freeze(), payload);
                }
                assert!(local.pending_queue.is_empty());
                assert!(local.inflight_queue.is_empty());
                if shutdown {
                    assert_eq!(local.state(), AssociationState::Closed);
                    assert_eq!(remote.state(), AssociationState::Closed);
                    assert!(remote.poll_endpoint_event().is_some());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn simultaneous_shutdown_retries_lost_forward_tsn_before_draining_mixed_streams() -> Result<()> {
    for first_tsn in [100, u32::MAX - 2] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        local.my_next_tsn = first_tsn;
        local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
        remote.peer_last_tsn = first_tsn.wrapping_sub(1);
        local.cwnd = 65536;
        local.ack_mode = AckMode::NoDelay;
        remote.ack_mode = AckMode::NoDelay;
        let abandoned = Bytes::from(vec![0xa5; local.max_payload_size as usize * 3 + 17]);
        local
            .stream(1)?
            .set_reliability_params(false, ReliabilityType::Rexmit, 0)?;
        local
            .stream(1)?
            .write_sctp(now, &abandoned, PayloadProtocolIdentifier::Binary)?;
        local
            .stream(1)?
            .set_reliability_params(false, ReliabilityType::Reliable, 0)?;
        let ordered_tail = [
            Bytes::from_static(b"first command"),
            Bytes::from_static(b"last command"),
        ];
        for payload in &ordered_tail {
            local
                .stream(1)?
                .write_sctp(now, payload, PayloadProtocolIdentifier::Binary)?;
        }
        let other_stream = Bytes::from_static(b"other stream gameplay");
        local
            .open_stream(3, PayloadProtocolIdentifier::Binary)?
            .write_sctp(now, &other_stream, PayloadProtocolIdentifier::Binary)?;
        let reverse = Bytes::from_static(b"reverse gameplay");
        remote
            .stream(1)?
            .write_sctp(now, &reverse, PayloadProtocolIdentifier::Binary)?;
        let initial = local.gather_outbound(now).0;
        let missing_tsn = first_tsn.wrapping_add(1);
        let local_tail_tsn = local.my_next_tsn.wrapping_sub(1);
        let remote_tail_tsn = remote.my_next_tsn;
        let mut dropped_data = 0;
        for raw in initial {
            let packet = Packet::unmarshal(&raw)?;
            if packet.chunks.iter().any(|chunk| {
                chunk
                    .as_any()
                    .downcast_ref::<ChunkPayloadData>()
                    .is_some_and(|data| data.tsn == missing_tsn)
            }) {
                assert_eq!(
                    packet.chunks.len(),
                    1,
                    "drop only the missing DATA fragment"
                );
                dropped_data += 1;
            } else {
                remote.handle_inbound(packet, now)?;
            }
        }
        assert_eq!(dropped_data, 1);
        assert!(remote.stream(1)?.read_sctp()?.is_none());
        local.shutdown()?;
        remote.shutdown()?;
        assert_eq!(local.state(), AssociationState::ShutdownPending);
        assert_eq!(remote.state(), AssociationState::ShutdownPending);
        for endpoint in [&mut local, &mut remote] {
            assert_eq!(
                endpoint.stream(1)?.write(now, b"late"),
                Err(Error::ErrStreamClosed)
            );
        }
        let mut forwards = vec![];
        for step in 0..200 {
            let at = now + Duration::from_millis(step * 100);
            local.handle_timeout(at);
            remote.handle_timeout(at);
            for raw in local.gather_outbound(at).0 {
                let packet = Packet::unmarshal(&raw)?;
                if let Some(forward) = packet
                    .chunks
                    .iter()
                    .find_map(|chunk| chunk.as_any().downcast_ref::<ChunkForwardTsn>())
                {
                    forwards.push((
                        forward.new_cumulative_tsn,
                        forward
                            .streams
                            .iter()
                            .map(|stream| (stream.identifier, stream.sequence))
                            .collect::<Vec<_>>(),
                    ));
                    if forwards.len() == 1 {
                        assert_eq!(packet.chunks.len(), 1, "drop only FORWARD TSN");
                        continue;
                    }
                }
                if packet.chunks.iter().any(|chunk| {
                    chunk.as_any().is::<ChunkShutdown>() || chunk.as_any().is::<ChunkShutdownAck>()
                }) {
                    assert!(local.pending_queue.is_empty());
                    assert!(local.inflight_queue.is_empty());
                    assert_eq!(local.cumulative_tsn_ack_point, local_tail_tsn);
                }
                remote.handle_inbound(packet, at)?;
            }
            for raw in remote.gather_outbound(at).0 {
                let packet = Packet::unmarshal(&raw)?;
                if packet.chunks.iter().any(|chunk| {
                    chunk.as_any().is::<ChunkShutdown>() || chunk.as_any().is::<ChunkShutdownAck>()
                }) {
                    assert!(remote.pending_queue.is_empty());
                    assert!(remote.inflight_queue.is_empty());
                    assert_eq!(remote.cumulative_tsn_ack_point, remote_tail_tsn);
                }
                local.handle_inbound(packet, at)?;
            }
            if local.state() == AssociationState::Closed
                && remote.state() == AssociationState::Closed
            {
                break;
            }
        }
        assert!(
            forwards.len() >= 2,
            "a lost FORWARD TSN must actually be retried"
        );
        assert_eq!(forwards[0], forwards[1]);
        assert_eq!(forwards[0].1, vec![(1, 0)]);
        assert_eq!(local.state(), AssociationState::Closed);
        assert_eq!(remote.state(), AssociationState::Closed);
        assert!(
            local.poll_endpoint_event().is_none(),
            "reverse input remains unread"
        );
        assert!(
            remote.poll_endpoint_event().is_none(),
            "forward input remains unread"
        );
        assert_eq!(
            local
                .stream(1)?
                .read_sctp()?
                .unwrap()
                .to_payload(100)?
                .freeze(),
            reverse
        );
        for expected in ordered_tail {
            assert_eq!(
                remote
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(100)?
                    .freeze(),
                expected,
                "the abandoned fragmented message must not reach the application"
            );
        }
        assert_eq!(
            remote
                .stream(3)?
                .read_sctp()?
                .unwrap()
                .to_payload(100)?
                .freeze(),
            other_stream
        );
        for endpoint in [&mut local, &mut remote] {
            assert!(endpoint.streams.is_empty());
            assert!(endpoint.pending_queue.is_empty());
            assert!(endpoint.inflight_queue.is_empty());
            assert!(endpoint.poll_timeout().is_none());
            assert!(matches!(
                endpoint.poll_endpoint_event(),
                Some(EndpointEvent(EndpointEventInner::Drained))
            ));
            assert!(endpoint.poll_endpoint_event().is_none());
        }
    }
    Ok(())
}

// Issue #1197: ordered stream reports cannot exceed the recovery packet budget.
#[test]
fn shutdown_forward_tsn_fits_mtu_across_many_ordered_streams() -> Result<()> {
    for (mtu, first_tsn) in [
        (64, 100),
        (64, u32::MAX - 8),
        (1200, 100),
        (1200, u32::MAX - 8),
    ] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        local.mtu = mtu;
        let stream_count = u16::try_from((mtu - 20) / 4 + 5).unwrap();
        local.my_next_tsn = first_tsn;
        local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
        remote.peer_last_tsn = first_tsn.wrapping_sub(1);
        local.cwnd = 65536;
        remote.ack_mode = AckMode::NoDelay;
        for id in 1..=stream_count {
            let mut stream = if id == 1 {
                local.stream(id)?
            } else {
                local.open_stream(id, PayloadProtocolIdentifier::Binary)?
            };
            stream.set_reliability_params(false, ReliabilityType::Rexmit, 0)?;
            stream.write(now, b"lost")?;
        }
        let lost = transmitted_data(&local.gather_outbound(now).0);
        assert_eq!(lost.len(), usize::from(stream_count));
        for id in 1..=stream_count {
            let mut stream = local.stream(id)?;
            stream.set_reliability_params(false, ReliabilityType::Reliable, 0)?;
            stream.write(now, &id.to_be_bytes())?;
        }
        local.shutdown()?;
        let mut forwards = vec![];
        for step in 0..200 {
            let at = now + Duration::from_millis(step * 100);
            local.handle_timeout(at);
            remote.handle_timeout(at);
            for raw in local.gather_outbound(at).0 {
                assert!(
                    raw.len() <= local.mtu as usize,
                    "recovery packet has {} bytes for mtu {}",
                    raw.len(),
                    local.mtu
                );
                let packet = Packet::unmarshal(&raw)?;
                if let Some(forward) = packet
                    .chunks
                    .iter()
                    .find_map(|chunk| chunk.as_any().downcast_ref::<ChunkForwardTsn>())
                {
                    forwards.push((
                        forward.new_cumulative_tsn,
                        forward
                            .streams
                            .iter()
                            .map(|stream| (stream.identifier, stream.sequence))
                            .collect::<Vec<_>>(),
                    ));
                    if forwards.len() == 1 {
                        assert_eq!(packet.chunks.len(), 1);
                        continue;
                    }
                }
                remote.handle_inbound(packet, at)?;
            }
            for raw in remote.gather_outbound(at).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
            if local.state() == AssociationState::Closed
                && remote.state() == AssociationState::Closed
            {
                break;
            }
        }
        assert!(
            forwards.len() >= 3,
            "stream progress must span multiple reports and retry the lost first report"
        );
        assert_eq!(forwards[0], forwards[1]);
        assert_eq!(local.state(), AssociationState::Closed);
        assert_eq!(remote.state(), AssociationState::Closed);
        for id in 1_u16..=stream_count {
            assert_eq!(
                remote
                    .stream(id)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(2)?
                    .freeze(),
                Bytes::copy_from_slice(&id.to_be_bytes())
            );
            assert!(remote.stream(id).is_err());
        }
        assert!(remote.poll_endpoint_event().is_some());
        assert!(remote.poll_endpoint_event().is_none());
    }
    Ok(())
}

#[test]
fn shutdown_forward_tsn_budget_preserves_fragment_and_stream_prefix() -> Result<()> {
    for first_tsn in [100, u32::MAX - 2] {
        let (mut local, _) = reset_pair();
        local.mtu = 24;
        local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        local.advanced_peer_tsn_ack_point = first_tsn.wrapping_add(4);
        for (offset, id, unordered, sequence) in [
            (0, 1, false, 4),
            (1, 2, true, 0),
            (2, 1, false, 4),
            (3, 1, false, 5),
            (4, 3, false, 0),
        ] {
            local.inflight_queue.push_no_check(ChunkPayloadData {
                tsn: first_tsn.wrapping_add(offset),
                stream_identifier: id,
                stream_sequence_number: sequence,
                unordered,
                abandoned: true,
                ..Default::default()
            });
            local.note_abandoned_for_forward_tsn(unordered, id, sequence);
        }
        let report = local.create_forward_tsn();
        assert_eq!(report.new_cumulative_tsn, first_tsn.wrapping_add(3));
        assert_eq!(report.streams.len(), 1);
        assert_eq!(report.streams[0].identifier, 1);
        assert_eq!(report.streams[0].sequence, 5);
        assert_eq!(local.marshal_control_chunk(&report)?.len(), 24);
        local.reconfigs.insert(
            1,
            ChunkReconfig {
                param_a: Some(Box::new(ParamOutgoingResetRequest {
                    sender_last_tsn: first_tsn.wrapping_add(2),
                    stream_identifiers: vec![1],
                    ..Default::default()
                })),
                ..Default::default()
            },
        );
        let fenced = local.create_forward_tsn();
        assert_eq!(fenced.new_cumulative_tsn, first_tsn.wrapping_add(2));
        assert_eq!(fenced.streams.len(), 1);
        assert_eq!(fenced.streams[0].identifier, 1);
        assert_eq!(fenced.streams[0].sequence, 4);
    }
    for (mtu, budget) in [(20, 0), (24, 1), (28, 2), (u32::MAX, 16381)] {
        let (mut local, _) = reset_pair();
        local.mtu = mtu;
        local.cumulative_tsn_ack_point = 0;
        local.advanced_peer_tsn_ack_point = 17000;
        for id in 1_u16..=17000 {
            local.inflight_queue.push_no_check(ChunkPayloadData {
                tsn: u32::from(id),
                stream_identifier: id,
                abandoned: true,
                ..Default::default()
            });
            local.note_abandoned_for_forward_tsn(false, id, 0);
        }
        let report = local.create_forward_tsn();
        assert_eq!(report.streams.len(), budget);
        assert_eq!(report.new_cumulative_tsn as usize, budget);
        let raw = local.marshal_control_chunk(&report)?;
        assert!(raw.len() <= mtu as usize);
        assert!(raw.len() - COMMON_HEADER_SIZE as usize <= u16::MAX as usize);
        let decoded = Packet::unmarshal(&raw)?;
        assert_eq!(decoded.chunks.len(), 1);
        assert_eq!(decoded.chunks[0].value_length(), report.value_length());
    }
    Ok(())
}

// Issue #1199: receive recovery follows incoming DATA, not local send settings.
#[test]
fn shutdown_forward_tsn_recovers_ordered_data_with_unordered_local_sends() -> Result<()> {
    forward_tsn_recovers_opposite_ordering(false)
}

#[test]
fn shutdown_forward_tsn_reclaims_unordered_data_with_ordered_local_sends() -> Result<()> {
    forward_tsn_recovers_opposite_ordering(true)
}

fn forward_tsn_recovers_opposite_ordering(unordered: bool) -> Result<()> {
    for shutdown in [false, true] {
        for first_tsn in [100, u32::MAX - 1] {
            let (mut local, mut remote) = reset_pair();
            let now = Instant::now();
            let payload_type = PayloadProtocolIdentifier::Binary;
            local.my_next_tsn = first_tsn;
            local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            remote.peer_last_tsn = first_tsn.wrapping_sub(1);
            local.cwnd = 65536;
            remote.ack_mode = AckMode::NoDelay;
            local.ack_mode = AckMode::NoDelay;
            local
                .stream(1)?
                .set_reliability_params(unordered, ReliabilityType::Rexmit, 0)?;
            remote
                .stream(1)?
                .set_reliability_params(!unordered, ReliabilityType::Reliable, 0)?;
            let abandoned = Bytes::from(vec![0xa5; local.max_payload_size as usize * 3 + 17]);
            local.stream(1)?.write_sctp(now, &abandoned, payload_type)?;
            local
                .stream(1)?
                .set_reliability_params(unordered, ReliabilityType::Reliable, 0)?;
            let successor = Bytes::from_static(b"exact surviving gameplay");
            local.stream(1)?.write_sctp(now, &successor, payload_type)?;
            local
                .stream(1)?
                .set_reliability_params(!unordered, ReliabilityType::Reliable, 0)?;
            let mixed_successor = Bytes::from_static(b"other ordering survives too");
            local
                .stream(1)?
                .write_sctp(now, &mixed_successor, payload_type)?;
            let expected_bytes = successor.len() + mixed_successor.len();
            let missing_tsn = first_tsn.wrapping_add(1);
            let mut dropped = 0;
            for raw in local.gather_outbound(now).0 {
                let packet = Packet::unmarshal(&raw)?;
                if packet.chunks.iter().any(|chunk| {
                    chunk
                        .as_any()
                        .downcast_ref::<ChunkPayloadData>()
                        .is_some_and(|data| data.tsn == missing_tsn)
                }) {
                    assert_eq!(packet.chunks.len(), 1, "drop only one DATA fragment");
                    dropped += 1;
                } else {
                    remote.handle_inbound(packet, now)?;
                }
            }
            assert_eq!(dropped, 1);
            assert!(
                remote
                    .streams
                    .get(&1)
                    .unwrap()
                    .get_num_bytes_in_reassembly_queue()
                    > expected_bytes
            );
            if shutdown {
                local.shutdown()?;
            }
            let mut forwards = 0;
            for step in 0..100 {
                let at = now + Duration::from_millis(step * 100);
                local.handle_timeout(at);
                remote.handle_timeout(at);
                for raw in local.gather_outbound(at).0 {
                    let packet = Packet::unmarshal(&raw)?;
                    forwards += packet
                        .chunks
                        .iter()
                        .filter(|chunk| chunk.as_any().is::<ChunkForwardTsn>())
                        .count();
                    remote.handle_inbound(packet, at)?;
                }
                for raw in remote.gather_outbound(at).0 {
                    local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                if forwards > 0
                    && local.inflight_queue.is_empty()
                    && (!shutdown || remote.state() == AssociationState::Closed)
                {
                    break;
                }
            }
            assert!(forwards > 0, "the lost fragment must require FORWARD TSN");
            assert_eq!(
                remote
                    .streams
                    .get(&1)
                    .unwrap()
                    .get_num_bytes_in_reassembly_queue(),
                expected_bytes,
                "unordered={unordered}, shutdown={shutdown}, first_tsn={first_tsn}: abandoned fragments must release receive credit"
            );
            let mut received = vec![];
            for _ in 0..2 {
                received.push(
                    remote
                        .stream(1)?
                        .read_sctp()?
                        .expect("FORWARD TSN must preserve both exact successors")
                        .to_payload(100)?
                        .freeze(),
                );
            }
            received.sort();
            let mut expected = vec![successor, mixed_successor];
            expected.sort();
            assert_eq!(received, expected);
            assert!(remote.stream(1).is_err() || remote.stream(1)?.read_sctp()?.is_none());
            assert!(local.inflight_queue.is_empty());
            assert!(local.pending_queue.is_empty());
            if shutdown {
                assert_eq!(local.state(), AssociationState::Closed);
                assert_eq!(remote.state(), AssociationState::Closed);
                assert!(remote.streams.is_empty());
                assert!(matches!(
                    remote.poll_endpoint_event(),
                    Some(EndpointEvent(EndpointEventInner::Drained))
                ));
                assert!(remote.poll_timeout().is_none());
            }
        }
    }
    Ok(())
}

// Issues #1205/#1206: every fragment must fit both DATA framing and padding.
fn fragment_budget_shutdown_history(payload_budget: u32, size: usize) -> Result<()> {
    let (_, mut remote) = reset_pair();
    let now = Instant::now();
    let mut config = crate::EndpointConfig::default();
    config.max_payload_size(payload_budget);
    let mut local = Association::new(
        None,
        Arc::new(TransportConfig::default()),
        config.get_max_payload_size(),
        0,
        "127.0.0.1:5000".parse().unwrap(),
        "127.0.0.1:5001".parse().unwrap(),
        TransportProtocol::UDP,
        now,
    );
    local.control_queue.clear();
    local.timers.stop(Timer::T1Init);
    local.set_state(AssociationState::Established);
    local.open_stream(1, PayloadProtocolIdentifier::Binary)?;
    local.peer_last_tsn = remote.my_next_tsn.wrapping_sub(1);
    remote.peer_last_tsn = local.my_next_tsn.wrapping_sub(1);
    local.rwnd = 1_000_000;
    assert_eq!(config.get_max_payload_size(), payload_budget);
    remote.max_receive_buffer_size = 1_000_000;
    remote.ack_mode = AckMode::NoDelay;
    let expected = Bytes::from((0..size).map(|n| (n % 251) as u8).collect::<Vec<_>>());
    let tail = Bytes::from_static(b"after fragmented gameplay");
    local
        .stream(1)?
        .write_sctp(now, &expected, PayloadProtocolIdentifier::Binary)?;
    local
        .stream(1)?
        .write_sctp(now, &tail, PayloadProtocolIdentifier::Binary)?;
    local.shutdown()?;
    let mut dropped = false;
    for step in 0..100 {
        let at = now + Duration::from_millis(step * 100);
        local.handle_timeout(at);
        remote.handle_timeout(at);
        for raw in local.gather_outbound(at).0 {
            let packet = Packet::unmarshal(&raw)?;
            let mut data_packet = false;
            for chunk in &packet.chunks {
                if let Some(data) = chunk.as_any().downcast_ref::<ChunkPayloadData>() {
                    data_packet = true;
                    assert!(
                        data.user_data.len() + DATA_CHUNK_HEADER_SIZE as usize
                            <= usize::from(u16::MAX),
                        "budget {payload_budget}"
                    );
                }
            }
            if data_packet {
                assert!(
                    raw.len() <= local.mtu as usize,
                    "budget {payload_budget}: {} > {}",
                    raw.len(),
                    local.mtu
                );
                // Require recovery through the retransmission packet path too.
                if !dropped {
                    dropped = true;
                    continue;
                }
            }
            remote.handle_inbound(packet, at)?;
        }
        for raw in remote.gather_outbound(at).0 {
            local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
        }
        if local.state() == AssociationState::Closed && remote.state() == AssociationState::Closed {
            break;
        }
    }
    assert!(dropped);
    if payload_budget == u32::MAX {
        assert!(
            local.stats.get_num_t3timeouts() > 0,
            "the lost complete DATA burst requires timer recovery"
        );
    }
    assert_eq!(
        local.state(),
        AssociationState::Closed,
        "budget {payload_budget}"
    );
    assert_eq!(
        remote.state(),
        AssociationState::Closed,
        "budget {payload_budget}"
    );
    assert!(local.inflight_queue.is_empty());
    assert!(local.pending_queue.is_empty());
    assert!(remote.poll_endpoint_event().is_none());
    assert_eq!(
        remote
            .stream(1)?
            .read_sctp()?
            .unwrap()
            .to_payload(200_000)?
            .freeze(),
        expected,
        "budget {payload_budget}"
    );
    assert_eq!(
        remote
            .stream(1)?
            .read_sctp()?
            .unwrap()
            .to_payload(200_000)?
            .freeze(),
        tail
    );
    assert!(remote.poll_endpoint_event().is_some());
    assert!(remote.poll_endpoint_event().is_none());
    Ok(())
}

#[test]
fn shutdown_large_fragment_budget_preserves_wire_lengths_and_exact_gameplay() -> Result<()> {
    // DATA's 16-bit length includes its 16-byte header. The application
    // message may exceed one chunk even with a large packet budget.
    for payload_budget in [65_516u32, 65_519, 65_520, 65_535, 65_536, u32::MAX] {
        fragment_budget_shutdown_history(payload_budget, 131_039)?;
    }
    Ok(())
}

#[test]
fn shutdown_unaligned_fragment_budget_fits_every_data_packet() -> Result<()> {
    // Include aligned budgets and the unchanged default as positive controls.
    for payload_budget in [4u32, 5, 6, 7, 8, 9, 1156, 1159, 1160, 1161, 1162, 1163] {
        fragment_budget_shutdown_history(payload_budget, payload_budget as usize * 3 + 1)?;
    }
    Ok(())
}

#[test]
fn shutdown_full_width_congestion_growth_keeps_send_credit() -> Result<()> {
    for (threshold, partial, acknowledged, pending, fast_recovery, window, remainder) in [
        (200, 0, 50, true, false, 150, 0),
        (200, 0, 50, true, true, 100, 0),
        (200, 0, 50, false, false, 100, 0),
        (50, 10, 20, true, false, 100, 30),
        (50, 10, 90, true, false, 200, 0),
        (50, 10, 100, true, false, 200, 10),
        (50, 90, 20, false, false, 100, 110),
    ] {
        let mut sender = timed_test_association();
        let now = Instant::now();
        if pending {
            sender
                .open_stream(1, PayloadProtocolIdentifier::Binary)?
                .write(now, b"queued")?;
        }
        sender.mtu = 100;
        sender.cwnd = 100;
        sender.ssthresh = threshold;
        sender.partial_bytes_acked = partial;
        sender.in_fast_recovery = fast_recovery;
        sender.on_cumulative_tsn_ack_point_advanced(acknowledged, now);
        assert_eq!(
            (sender.cwnd, sender.partial_bytes_acked),
            (window, remainder),
            "threshold={threshold} partial={partial} acknowledged={acknowledged} pending={pending} recovery={fast_recovery}"
        );
    }
    let mut sender = timed_test_association();
    let now = Instant::now();
    sender
        .open_stream(1, PayloadProtocolIdentifier::Binary)?
        .write(now, b"queued gameplay")?;
    sender.mtu = u32::MAX;
    sender.cwnd = u32::MAX - 1;
    sender.ssthresh = u32::MAX;
    sender.on_cumulative_tsn_ack_point_advanced(100, now);
    assert_eq!(sender.cwnd, u32::MAX, "slow start cannot wrap send credit");
    sender.cwnd = u32::MAX - 1;
    sender.ssthresh = 0;
    sender.partial_bytes_acked = u32::MAX - 2;
    sender.on_cumulative_tsn_ack_point_advanced(100, now);
    assert_eq!(
        sender.cwnd,
        u32::MAX,
        "congestion avoidance cannot wrap credit"
    );
    assert_eq!(
        sender.partial_bytes_acked, 99,
        "retain the exact additive-increase remainder"
    );
    sender.gather_outbound(now);
    let first = sender.cumulative_tsn_ack_point.wrapping_add(1);
    sender.inflight_queue.get_mut(first).unwrap().miss_indicator = 2;
    sender.process_fast_retransmission(
        sender.cumulative_tsn_ack_point,
        first.wrapping_add(1),
        false,
    )?;
    assert!(sender.in_fast_recovery);
    assert_eq!(sender.cwnd, u32::MAX);
    assert_eq!(sender.ssthresh, u32::MAX);
    assert_eq!(sender.partial_bytes_acked, 0);
    Ok(())
}

#[test]
fn shutdown_zero_window_probe_delivers_fragmented_tail_after_abandonment() -> Result<()> {
    for shutdown in [false, true] {
        for first_tsn in [100, u32::MAX - 1] {
            let (mut local, mut remote) = reset_pair();
            let now = Instant::now();
            local.my_next_tsn = first_tsn;
            local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            remote.peer_last_tsn = first_tsn.wrapping_sub(1);
            local.use_forward_tsn = true;
            remote.use_forward_tsn = true;
            local.ack_mode = AckMode::NoDelay;
            remote.ack_mode = AckMode::NoDelay;
            local.cwnd = 10 * local.mtu;
            remote.max_receive_buffer_size = local.max_payload_size;
            local
                .stream(1)?
                .set_reliability_params(false, ReliabilityType::Rexmit, 0)?;
            local.open_stream(2, PayloadProtocolIdentifier::Binary)?;
            local.stream(1)?.write(now, b"obsolete update")?;
            let expected = Bytes::from(vec![0x5a; local.max_payload_size as usize + 17]);
            local
                .stream(2)?
                .write_sctp(now, &expected, PayloadProtocolIdentifier::Binary)?;
            let original = transmitted_data(&local.gather_outbound(now).0);
            assert_eq!(original.len(), 3);
            assert!(original[1].beginning_fragment);
            let first_packet = local
                .create_packet(vec![Box::new(original[1].clone())])
                .marshal()?;
            remote.handle_inbound(Packet::unmarshal(&first_packet)?, now)?;
            assert_eq!(remote.get_my_receiver_window_credit(), 0);
            for raw in remote.gather_outbound(now).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            assert_eq!(local.rwnd, 0);
            assert!(
                local
                    .inflight_queue
                    .get(original[1].tsn)
                    .unwrap()
                    .acknowledged
            );
            if shutdown {
                local.shutdown()?;
            }
            let retry_at = local.timers.get(Timer::T3RTX).unwrap();
            local.handle_timeout(retry_at);
            let packets = local.gather_outbound(retry_at).0;
            let retry = transmitted_data(&packets);
            assert_eq!(
                retry.len(),
                1,
                "shutdown={shutdown}, first_tsn={first_tsn}: a retained abandoned or gap-acked prefix must not block the live probe"
            );
            assert_eq!(retry[0].tsn, original[2].tsn);
            for raw in &packets {
                let packet = Packet::unmarshal(raw)?;
                if packet
                    .chunks
                    .iter()
                    .any(|chunk| chunk.as_any().is::<ChunkForwardTsn>())
                {
                    remote.handle_inbound(packet, retry_at)?;
                }
            }
            assert_eq!(remote.peer_last_tsn, original[1].tsn);
            assert_eq!(remote.get_my_receiver_window_credit(), 0);
            for raw in &packets {
                let packet = Packet::unmarshal(raw)?;
                if packet
                    .chunks
                    .iter()
                    .any(|chunk| chunk.as_any().is::<ChunkPayloadData>())
                {
                    remote.handle_inbound(packet, retry_at)?;
                }
            }
            assert_eq!(
                remote
                    .stream(2)?
                    .read_sctp()?
                    .unwrap()
                    .into_payload(expected.len())?
                    .freeze(),
                expected
            );
            assert!(remote.stream(2)?.read_sctp()?.is_none());
            assert!(
                transmitted_data(&local.gather_outbound(retry_at).0).is_empty(),
                "a second poll must not send another probe before its timer"
            );
            for step in 0..100 {
                let at = retry_at + Duration::from_millis(step * 100);
                local.handle_timeout(at);
                remote.handle_timeout(at);
                for raw in remote.gather_outbound(at).0 {
                    local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                for raw in local.gather_outbound(at).0 {
                    remote.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                if local.inflight_queue.is_empty()
                    && (!shutdown
                        || local.state() == AssociationState::Closed
                            && remote.state() == AssociationState::Closed)
                {
                    break;
                }
            }
            assert!(local.pending_queue.is_empty());
            assert!(local.inflight_queue.is_empty());
            assert_eq!(
                local
                    .streams
                    .get(&2)
                    .map_or(0, |stream| stream.buffered_amount),
                0
            );
            if shutdown {
                for endpoint in [&mut local, &mut remote] {
                    assert_eq!(endpoint.state(), AssociationState::Closed);
                    assert!(endpoint.poll_timeout().is_none());
                    assert!(endpoint.poll_endpoint_event().is_some());
                    assert!(endpoint.poll_endpoint_event().is_none());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn shutdown_zero_window_pending_tail_recovers_a_lost_window_update() -> Result<()> {
    for shutdown in [false, true] {
        for first_tsn in [100, u32::MAX - 1] {
            let (mut local, mut remote) = reset_pair();
            let now = Instant::now();
            local.my_next_tsn = first_tsn;
            local.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            local.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            remote.peer_last_tsn = first_tsn.wrapping_sub(1);
            local.ack_mode = AckMode::NoDelay;
            remote.ack_mode = AckMode::NoDelay;
            remote.max_receive_buffer_size = local.max_payload_size;
            local.rwnd = local.max_payload_size;
            let filler = Bytes::from(vec![0x11; local.max_payload_size as usize]);
            local
                .stream(1)?
                .write_sctp(now, &filler, PayloadProtocolIdentifier::Binary)?;
            for raw in local.gather_outbound(now).0 {
                remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            for raw in remote.gather_outbound(now).0 {
                local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
            }
            assert_eq!(remote.get_my_receiver_window_credit(), 0);
            assert_eq!(local.rwnd, 0);
            assert!(local.inflight_queue.is_empty());
            local
                .open_stream(2, PayloadProtocolIdentifier::Binary)?
                .set_reliability_params(false, ReliabilityType::Rexmit, 0)?;
            local.open_stream(3, PayloadProtocolIdentifier::Binary)?;
            local.stream(2)?.write(now, b"obsolete")?;
            let expected = Bytes::from(vec![0xa5; local.max_payload_size as usize + 17]);
            local
                .stream(3)?
                .write_sctp(now, &expected, PayloadProtocolIdentifier::Binary)?;
            let lost = transmitted_data(&local.gather_outbound(now).0);
            assert_eq!(lost.len(), 1);
            assert_eq!(lost[0].stream_identifier, 2);
            assert_eq!(local.pending_queue.len(), 2);
            assert_eq!(
                remote
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .into_payload(filler.len())?
                    .freeze(),
                filler
            );
            let lost_window_update = remote.gather_outbound(now).0;
            assert!(!lost_window_update.is_empty());
            assert_eq!(
                remote.get_my_receiver_window_credit(),
                local.max_payload_size
            );
            assert_eq!(local.rwnd, 0);
            if shutdown {
                local.shutdown()?;
            }
            let retry_at = local.timers.get(Timer::T3RTX).unwrap();
            local.handle_timeout(retry_at);
            let packets = local.gather_outbound(retry_at).0;
            let probe = transmitted_data(&packets);
            assert_eq!(
                probe.len(),
                1,
                "shutdown={shutdown}, first_tsn={first_tsn}: pending DATA must recover the lost window update after the old probe expires"
            );
            assert_eq!(probe[0].stream_identifier, 3);
            assert!(probe[0].beginning_fragment);
            assert_eq!(local.pending_queue.len(), 1);
            assert!(transmitted_data(&local.gather_outbound(retry_at).0).is_empty());
            for forward in [true, false] {
                for raw in &packets {
                    let packet = Packet::unmarshal(raw)?;
                    if packet
                        .chunks
                        .iter()
                        .any(|chunk| chunk.as_any().is::<ChunkForwardTsn>())
                        == forward
                    {
                        remote.handle_inbound(packet, retry_at)?;
                    }
                }
            }
            let mut received = Vec::new();
            for step in 0..100 {
                let at = retry_at + Duration::from_millis(step * 100);
                local.handle_timeout(at);
                remote.handle_timeout(at);
                for raw in remote.gather_outbound(at).0 {
                    local.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                for raw in local.gather_outbound(at).0 {
                    remote.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                if remote.streams.contains_key(&3) {
                    while let Some(message) = remote.stream(3)?.read_sctp()? {
                        received.push(message.into_payload(expected.len())?.freeze());
                    }
                }
                if local.inflight_queue.is_empty()
                    && (!shutdown
                        || local.state() == AssociationState::Closed
                            && remote.state() == AssociationState::Closed)
                {
                    break;
                }
            }
            assert_eq!(received, vec![expected]);
            assert!(local.pending_queue.is_empty());
            assert!(local.inflight_queue.is_empty());
            if shutdown {
                for endpoint in [&mut local, &mut remote] {
                    assert_eq!(endpoint.state(), AssociationState::Closed);
                    assert!(endpoint.poll_timeout().is_none());
                    assert!(endpoint.poll_endpoint_event().is_some());
                    assert!(endpoint.poll_endpoint_event().is_none());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn shutdown_receive_gap_does_not_retain_delivered_payload_storage() -> Result<()> {
    for (shutdown, receiver_shutdown) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        for first_tsn in [100, u32::MAX - 2] {
            let (mut sender, mut receiver) = reset_pair();
            let now = Instant::now();
            sender.my_next_tsn = first_tsn;
            sender.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            sender.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            receiver.peer_last_tsn = first_tsn.wrapping_sub(1);
            sender.ack_mode = AckMode::NoDelay;
            receiver.ack_mode = AckMode::NoDelay;
            sender.cwnd = 64 * sender.mtu;
            sender.rwnd = sender.cwnd;
            let ppi = PayloadProtocolIdentifier::Binary;
            let missing = Bytes::from(vec![0xa5; sender.max_payload_size as usize]);
            sender.stream(1)?.write_sctp(now, &missing, ppi)?;
            sender.open_stream(2, ppi)?;
            let expected: Vec<_> = (0..16)
                .map(|index| Bytes::from(vec![index; sender.max_payload_size as usize]))
                .collect();
            for payload in &expected {
                sender.stream(2)?.write_sctp(now, payload, ppi)?;
            }
            if shutdown {
                sender.shutdown()?;
            }
            let mut packets = sender.gather_outbound(now).0.into_iter();
            if receiver_shutdown {
                receiver.shutdown()?;
            }
            let held = packets.next().expect("missing prefix packet");
            let mut outputs = Vec::new();
            for (index, raw) in packets.enumerate() {
                // A socket supplies independent receive storage. The sender's
                // zero-copy burst shares its backing allocation across packets.
                let raw = Bytes::copy_from_slice(&raw);
                let packet = Packet::unmarshal(&raw)?;
                assert_eq!(packet.chunks.len(), 1);
                let data = packet.chunks[0]
                    .as_any()
                    .downcast_ref::<ChunkPayloadData>()
                    .expect("one complete gameplay message per packet");
                assert_eq!(data.user_data, expected[index]);
                let pointer = data.user_data.as_ptr();
                receiver.handle_inbound(packet, now)?;
                receiver.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                drop(raw);
                let message = receiver
                    .stream(2)?
                    .read_sctp()?
                    .expect("ready other stream");
                assert!(
                    message.chunks[0].user_data.is_unique(),
                    "gap must not share payload storage"
                );
                let output = message.into_payload(65536)?;
                assert_eq!(
                    output.as_ptr(),
                    pointer,
                    "gap must not force a receive copy"
                );
                assert_eq!(output.as_ref(), expected[index].as_ref());
                outputs.push(output);
            }
            assert_eq!(outputs.len(), expected.len());
            assert_eq!(receiver.payload_queue.len(), expected.len());
            assert_eq!(receiver.payload_queue.get_num_bytes(), 0);
            assert_eq!(
                receiver.get_my_receiver_window_credit(),
                receiver.max_receive_buffer_size
            );
            // Duplicate detection and the exact gap report must survive application reads.
            let sack = receiver.create_selective_ack_chunk();
            assert_eq!(sack.cumulative_tsn_ack, first_tsn.wrapping_sub(1));
            assert_eq!(sack.gap_ack_blocks.len(), 1);
            assert_eq!(sack.gap_ack_blocks[0].start, 2);
            assert_eq!(sack.gap_ack_blocks[0].end, 17);
            assert_eq!(
                sack.duplicate_tsn,
                (1_u32..=16)
                    .map(|offset| first_tsn.wrapping_add(offset))
                    .collect::<Vec<_>>()
            );
            assert!(
                receiver.stream(2)?.read_sctp()?.is_none(),
                "duplicates cannot become gameplay"
            );
            sender.handle_sack(&sack, now)?;
            receiver.handle_inbound(Packet::unmarshal(&held)?, now)?;
            drop(held);
            assert_eq!(receiver.peer_last_tsn, first_tsn.wrapping_add(16));
            assert!(receiver.payload_queue.is_empty());
            assert_eq!(
                receiver
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .into_payload(65536)?
                    .as_ref(),
                missing.as_ref()
            );
            for _ in 0..8 {
                for raw in receiver.gather_outbound(now).0 {
                    sender.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                }
                for raw in sender.gather_outbound(now).0 {
                    receiver.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                }
            }
            if shutdown || receiver_shutdown {
                for endpoint in [&mut sender, &mut receiver] {
                    assert_eq!(endpoint.state(), AssociationState::Closed);
                    assert!(endpoint.poll_endpoint_event().is_some());
                    assert!(endpoint.poll_endpoint_event().is_none());
                }
            }
            for (output, expected) in outputs.iter().zip(&expected) {
                assert_eq!(
                    output.as_ref(),
                    expected.as_ref(),
                    "application owns bytes after closure"
                );
            }
        }
    }
    Ok(())
}

#[test]
#[ignore = "isolated receive-gap storage measurement; run in a fresh process"]
fn shutdown_receive_gap_storage_profile() -> Result<()> {
    let (mut sender, mut receiver) = reset_pair();
    let now = Instant::now();
    let ppi = PayloadProtocolIdentifier::Binary;
    sender.cwnd = 64 * 1024 * 1024;
    sender.rwnd = sender.cwnd;
    receiver.max_receive_buffer_size = sender.cwnd;
    sender
        .stream(1)?
        .write_sctp(now, &Bytes::from(vec![0xff; 1024]), ppi)?;
    sender.open_stream(2, ppi)?;
    for index in 0..16384_u32 {
        let mut payload = vec![0; 1024];
        payload[..4].copy_from_slice(&index.to_be_bytes());
        sender.stream(2)?.write(now, &payload)?;
    }
    sender.shutdown()?;
    let mut packets = sender.gather_outbound(now).0.into_iter();
    let _held = packets.next().expect("hold the first TSN");
    let mut received = 0_u32;
    for raw in packets {
        // Match independent socket receive buffers, rather than send-burst slices.
        let raw = Bytes::copy_from_slice(&raw);
        receiver.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        drop(raw);
        while let Some(message) = receiver.stream(2)?.read_sctp()? {
            let payload = message.into_payload(1024)?;
            assert_eq!(&payload[..4], &received.to_be_bytes());
            assert!(payload[4..].iter().all(|byte| *byte == 0));
            received += 1;
        }
    }
    assert_eq!(received, 16384);
    assert_eq!(receiver.payload_queue.len(), received as usize);
    println!(
        "RECEIVE_GAP_STORAGE messages={received} retained_payload_bytes={}",
        receiver.payload_queue.get_num_bytes()
    );
    std::hint::black_box(receiver);
    Ok(())
}

#[test]
fn shutdown_released_send_storage_does_not_wait_for_missing_prefix() -> Result<()> {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct OwnedPayload(Vec<u8>, Arc<AtomicUsize>);
    impl AsRef<[u8]> for OwnedPayload {
        fn as_ref(&self) -> &[u8] {
            &self.0
        }
    }
    impl Drop for OwnedPayload {
        fn drop(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }

    for abandon in [false, true] {
        for first_tsn in [100, u32::MAX - 2] {
            let (mut sender, mut receiver) = reset_pair();
            let now = Instant::now();
            let drops = Arc::new(AtomicUsize::new(0));
            let ppi = PayloadProtocolIdentifier::Binary;
            sender.my_next_tsn = first_tsn;
            sender.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            sender.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            receiver.peer_last_tsn = first_tsn.wrapping_sub(1);
            sender.cwnd = 64 * sender.mtu;
            sender.rwnd = sender.cwnd;
            sender.ack_mode = AckMode::NoDelay;
            receiver.ack_mode = AckMode::NoDelay;
            sender.stream(1)?.write(now, b"missing prefix")?;
            let missing = sender.gather_outbound(now).0;
            sender.open_stream(2, ppi)?;
            if abandon {
                sender
                    .stream(2)?
                    .set_reliability_params(false, ReliabilityType::Rexmit, 0)?;
            }
            let expected: Vec<_> = (0..3000).map(|index| (index % 251) as u8).collect();
            sender.stream(2)?.write_sctp(
                now,
                &Bytes::from_owner(OwnedPayload(expected.clone(), Arc::clone(&drops))),
                ppi,
            )?;
            for raw in sender.gather_outbound(now).0 {
                if !abandon {
                    receiver.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                }
            }
            sender.stream(1)?.write(now, b"reliable successor")?;
            // Keep a successor admitted but unsent while the old storage retires.
            assert_eq!(drops.load(Ordering::SeqCst), 0);
            sender.shutdown()?;
            let at = if abandon {
                let at = sender.poll_timeout().expect("retry deadline");
                sender.handle_timeout(at);
                // T3 retires all fragments of the no-retry message.
                sender.gather_outbound(at);
                at
            } else {
                assert_eq!(
                    receiver
                        .stream(2)?
                        .read_sctp()?
                        .unwrap()
                        .into_payload(65536)?
                        .as_ref(),
                    expected.as_slice()
                );
                sender.handle_sack(&receiver.create_selective_ack_chunk(), now)?;
                now
            };
            assert_eq!(sender.cumulative_tsn_ack_point, first_tsn.wrapping_sub(1));
            assert!(!sender.inflight_queue.is_empty());
            assert_eq!(
                drops.load(Ordering::SeqCst),
                1,
                "released fragments must not retain caller storage behind a missing prefix"
            );
            for raw in missing {
                receiver.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
            assert_eq!(
                receiver
                    .stream(1)?
                    .read_sctp()?
                    .unwrap()
                    .into_payload(65536)?
                    .as_ref(),
                b"missing prefix"
            );
            let mut successors = Vec::new();
            for step in 0..80 {
                let at = at + Duration::from_millis(step * 100);
                sender.handle_timeout(at);
                receiver.handle_timeout(at);
                for raw in sender.gather_outbound(at).0 {
                    receiver.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                for raw in receiver.gather_outbound(at).0 {
                    sender.handle_inbound(Packet::unmarshal(&raw)?, at)?;
                }
                if receiver.streams.contains_key(&1) {
                    while let Some(message) = receiver.stream(1)?.read_sctp()? {
                        successors.push(message.into_payload(65536)?.freeze());
                    }
                }
                if sender.state() == AssociationState::Closed
                    && receiver.state() == AssociationState::Closed
                {
                    break;
                }
            }
            assert_eq!(successors, vec![Bytes::from_static(b"reliable successor")]);
            if abandon && receiver.streams.contains_key(&2) {
                assert!(receiver.stream(2)?.read_sctp()?.is_none());
            }
            for endpoint in [&mut sender, &mut receiver] {
                assert_eq!(endpoint.state(), AssociationState::Closed);
                assert!(endpoint.poll_endpoint_event().is_some());
                assert!(endpoint.poll_endpoint_event().is_none());
            }
            assert_eq!(drops.load(Ordering::SeqCst), 1);
        }
    }
    Ok(())
}

#[test]
fn shutdown_receive_credit_saturates_across_stream_and_reset_storage() -> Result<()> {
    for maximum in [0, 1, 65536, u32::MAX] {
        for (first, second, deferred) in [
            (0, 0, 0),
            (1, 2, 3),
            (u32::MAX as usize, 1, 0),
            (u32::MAX as usize / 2 + 1, u32::MAX as usize / 2 + 1, 0),
            (1, 1, usize::MAX),
        ] {
            let (mut receiver, _) = reset_pair();
            receiver.open_stream(2, PayloadProtocolIdentifier::Binary)?;
            receiver.max_receive_buffer_size = maximum;
            receiver
                .streams
                .get_mut(&1)
                .unwrap()
                .reassembly_queue
                .n_bytes = first;
            receiver
                .streams
                .get_mut(&2)
                .unwrap()
                .reassembly_queue
                .n_bytes = second;
            receiver.deferred_reset_bytes = deferred;
            let expected = u128::from(maximum)
                .saturating_sub(first as u128 + second as u128 + deferred as u128)
                as u32;
            assert_eq!(
                receiver.get_my_receiver_window_credit(),
                expected,
                "maximum={maximum}, queues={first}/{second}, deferred={deferred}"
            );
            receiver.shutdown()?;
            assert_eq!(
                receiver
                    .create_selective_ack_chunk()
                    .advertised_receiver_window_credit,
                expected
            );
        }
    }
    Ok(())
}

#[test]
#[cfg(target_pointer_width = "64")]
fn shutdown_receive_credit_keeps_large_accepted_tail_until_read() -> Result<()> {
    let (mut receiver, _) = reset_pair();
    let now = Instant::now();
    receiver.max_receive_buffer_size = u32::MAX;
    let body = Bytes::from(vec![0x5a; 65000]);
    let count = u32::MAX as usize / body.len() + 1;
    // Share physical storage to exercise over 4 GiB of logically accepted
    // DATA without requiring a 4 GiB test allocation. Every message has its
    // own valid TSN and ordered SSN; no queue counters are injected here.
    for index in 0..count {
        receiver.handle_data(&ChunkPayloadData {
            tsn: receiver.peer_last_tsn.wrapping_add(1),
            stream_identifier: 1,
            stream_sequence_number: index as u16,
            beginning_fragment: true,
            ending_fragment: true,
            payload_type: PayloadProtocolIdentifier::Binary,
            user_data: body.clone(),
            ..Default::default()
        })?;
    }
    assert_eq!(
        receiver.get_my_receiver_window_credit(),
        0,
        "a final admitted message may exceed remaining credit; it must not reopen the window"
    );
    assert_eq!(
        receiver.streams[&1].get_num_bytes_in_reassembly_queue(),
        count * body.len()
    );
    // A live graceful state must keep every accepted payload readable.
    receiver
        .open_stream(2, PayloadProtocolIdentifier::Binary)?
        .write(now, b"pending local command")?;
    receiver.shutdown()?;
    assert_eq!(receiver.state(), AssociationState::ShutdownPending);
    for index in 0..count {
        let message = receiver.stream(1)?.read_sctp()?.expect("accepted tail");
        assert_eq!(message.chunks.len(), 1);
        assert_eq!(message.chunks[0].user_data, body, "message {index}");
        let expected = (u32::MAX as usize).saturating_sub((count - index - 1) * body.len()) as u32;
        assert_eq!(receiver.get_my_receiver_window_credit(), expected);
    }
    assert_eq!(receiver.get_my_receiver_window_credit(), u32::MAX);
    assert!(receiver.stream(1)?.read_sctp()?.is_none());
    Ok(())
}

#[test]
fn simultaneous_shutdown_completes_before_queued_ack_replay() -> Result<()> {
    for replay_after_ack in [false, true] {
        let (mut local, mut remote) = reset_pair();
        let now = Instant::now();
        local.shutdown()?;
        remote.shutdown()?;
        let local_shutdown = local.gather_outbound(now).0;
        let remote_shutdown = remote.gather_outbound(now).0;
        for raw in &local_shutdown {
            remote.handle_inbound(Packet::unmarshal(raw)?, now)?;
        }
        let remote_ack = remote.gather_outbound(now).0;
        assert_eq!(remote.state(), AssociationState::ShutdownAckSent);
        if !replay_after_ack {
            for raw in &remote_shutdown {
                local.handle_inbound(Packet::unmarshal(raw)?, now)?;
            }
        }
        for raw in remote_ack {
            local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        if replay_after_ack {
            for raw in &remote_shutdown {
                local.handle_inbound(Packet::unmarshal(raw)?, now)?;
            }
        }
        let (complete, live) = local.gather_outbound(now);
        assert!(
            !live,
            "the first flush must complete the accepted shutdown ACK"
        );
        assert_eq!(complete.len(), 1);
        let packet = Packet::unmarshal(&complete[0])?;
        assert_eq!(packet.chunks.len(), 1);
        assert!(packet.chunks[0].as_any().is::<ChunkShutdownComplete>());
        remote.handle_inbound(packet, now)?;
        for endpoint in [&mut local, &mut remote] {
            assert_eq!(endpoint.state(), AssociationState::Closed);
            assert!(endpoint.poll_timeout().is_none());
            assert!(endpoint.poll_endpoint_event().is_some());
            assert!(endpoint.poll_endpoint_event().is_none());
            assert!(endpoint.poll_transmit(now).is_none());
        }
    }
    Ok(())
}

#[test]
fn shutdown_ack_completes_before_queued_timeout_retransmission() -> Result<()> {
    let (mut local, mut remote) = reset_pair();
    let now = Instant::now();
    local.shutdown()?;
    for raw in local.gather_outbound(now).0 {
        remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
    }
    let ack = remote.gather_outbound(now).0;
    let retry = local.poll_timeout().expect("shutdown retry armed");
    local.handle_timeout(retry);
    assert!(local.will_send_shutdown);
    for raw in ack {
        local.handle_inbound(Packet::unmarshal(&raw)?, retry)?;
    }
    let (complete, live) = local.gather_outbound(retry);
    assert!(!live);
    assert_eq!(complete.len(), 1);
    let packet = Packet::unmarshal(&complete[0])?;
    assert!(packet.chunks[0].as_any().is::<ChunkShutdownComplete>());
    remote.handle_inbound(packet, retry)?;
    for endpoint in [&mut local, &mut remote] {
        assert_eq!(endpoint.state(), AssociationState::Closed);
        assert!(endpoint.poll_timeout().is_none());
        assert!(endpoint.poll_endpoint_event().is_some());
        assert!(endpoint.poll_endpoint_event().is_none());
        assert!(endpoint.poll_transmit(retry).is_none());
    }
    Ok(())
}

// Regression #1239: terminal transport success takes precedence over reset recovery.
#[test]
fn shutdown_ack_preserves_unread_gameplay_before_due_reset_exhaustion() -> Result<()> {
    for retries in [0, 1, 5] {
        for simultaneous in [false, true] {
            for late_reply in [
                None,
                Some(ReconfigResult::ErrorRequestAlreadyInProgress),
                Some(ReconfigResult::InProgress),
            ] {
                let (mut local, mut remote) = reset_pair();
                local.timers = TimerTable::new(TimerConfig {
                    max_reconfig_retrans: retries,
                    ..TimerConfig::default()
                });
                local.ack_mode = AckMode::NoDelay;
                let mut now = Instant::now();
                let payload =
                    Bytes::from_static(b"accepted gameplay must survive terminal acknowledgment");
                remote
                    .open_stream(2, PayloadProtocolIdentifier::Binary)?
                    .write_sctp(now, &payload, PayloadProtocolIdentifier::Binary)?;
                for raw in remote.gather_outbound(now).0 {
                    local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                }
                for raw in local.gather_outbound(now).0 {
                    remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                }
                assert!(remote.inflight_queue.is_empty());
                local.send_reset_request(now, 1)?;
                let reset = reset_requests(&mut local, now).remove(0);
                let reset_sequence = request(&reset).reconfig_request_sequence_number;
                // Lose every reset request. The unrelated readable stream stays live.
                for _ in 0..retries {
                    now = local.poll_timeout().expect("reset retry is armed");
                    local.handle_timeout(now);
                    assert_eq!(reset_requests(&mut local, now).len(), 1);
                }
                let reset_deadline = local.poll_timeout().expect("final reset retry is armed");
                local.shutdown()?;
                let shutdown = local.gather_outbound(now).0;
                if simultaneous {
                    remote.shutdown()?;
                    for raw in remote.gather_outbound(now).0 {
                        local.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                    }
                }
                for raw in shutdown {
                    remote.handle_inbound(Packet::unmarshal(&raw)?, now)?;
                }
                for raw in remote.gather_outbound(now).0 {
                    // The ACK arrives on the same tick as the saved reset deadline.
                    local.handle_inbound(Packet::unmarshal(&raw)?, reset_deadline)?;
                }
                assert!(local.will_send_shutdown_complete);
                assert!(local.poll_timeout().is_none());
                // Accepted late reset replies can rearm recovery before the final flush.
                if let Some(result) = late_reply {
                    let reply = remote.create_packet(vec![Box::new(ChunkReconfig {
                        param_a: Some(response(reset_sequence, result)),
                        ..Default::default()
                    })]);
                    local.handle_inbound(Packet::unmarshal(&reply.marshal()?)?, reset_deadline)?;
                }
                // A driver can service a due timer after this ACK and before flushing.
                let callback_at = local.poll_timeout().unwrap_or(reset_deadline);
                local.handle_timeout(callback_at);
                assert!(local.poll_timeout().is_none());
                assert!(
                    !local
                        .events
                        .iter()
                        .any(|event| matches!(event, Event::AssociationFailed { .. })),
                    "retries={retries}, simultaneous={simultaneous}, late_reply={late_reply:?}: an accepted shutdown ACK must win over reset exhaustion"
                );
                let (complete, live) = local.gather_outbound(callback_at);
                assert!(!live);
                assert_eq!(complete.len(), 1);
                let packet = Packet::unmarshal(&complete[0])?;
                assert_eq!(packet.chunks.len(), 1);
                assert!(packet.chunks[0].as_any().is::<ChunkShutdownComplete>());
                remote.handle_inbound(packet, callback_at)?;
                assert!(
                    local.poll_endpoint_event().is_none(),
                    "unread gameplay owns endpoint lifetime"
                );
                assert_eq!(
                    local
                        .stream(2)?
                        .read_sctp()?
                        .unwrap()
                        .to_payload(1024)?
                        .freeze(),
                    payload
                );
                assert!(local.stream(2).is_err());
                for endpoint in [&mut local, &mut remote] {
                    assert_eq!(endpoint.state(), AssociationState::Closed);
                    assert!(endpoint.poll_timeout().is_none());
                    assert!(endpoint.poll_endpoint_event().is_some());
                    assert!(endpoint.poll_endpoint_event().is_none());
                    assert!(endpoint.poll_transmit(callback_at).is_none());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn data_ingress_counters_distinguish_acceptance_redundancy_and_refusal() -> Result<()> {
    for wrap in [false, true] {
        let (_, mut receiver) = reset_pair();
        if wrap {
            receiver.peer_last_tsn = u32::MAX - 1;
        }
        receiver.max_receive_buffer_size = 4;
        let first_tsn = receiver.peer_last_tsn.wrapping_add(1);
        let mut data = ChunkPayloadData {
            tsn: first_tsn,
            stream_identifier: 1,
            beginning_fragment: true,
            ending_fragment: true,
            user_data: Bytes::from_static(b"play"),
            ..Default::default()
        };
        receiver.handle_data(&data)?;
        receiver.handle_data(&data)?;
        data.tsn = first_tsn.wrapping_add(1);
        data.stream_sequence_number = 1;
        receiver.handle_data(&data)?;
        assert_eq!(receiver.stats.get_num_accepted_datas(), 1);
        assert_eq!(receiver.stats.get_num_redundant_datas(), 1);
        assert_eq!(receiver.stats.get_num_refused_datas(), 1);
        assert_eq!(
            receiver
                .stream(1)?
                .read_sctp()?
                .unwrap()
                .to_payload(4)?
                .freeze(),
            Bytes::from_static(b"play")
        );
        receiver.handle_data(&data)?;
        assert_eq!(
            receiver
                .stream(1)?
                .read_sctp()?
                .unwrap()
                .to_payload(4)?
                .freeze(),
            Bytes::from_static(b"play")
        );
        data.tsn = first_tsn.wrapping_add(2);
        data.stream_sequence_number = 2;
        receiver.handle_forward_tsn(&ChunkForwardTsn {
            new_cumulative_tsn: data.tsn,
            streams: vec![ChunkForwardTsnStream {
                identifier: 1,
                sequence: 2,
            }],
        })?;
        receiver.handle_data(&data)?;
        let mut stats = receiver.stats();
        assert_eq!(stats.get_num_datas(), 5);
        assert_eq!(stats.get_num_accepted_datas(), 2);
        assert_eq!(stats.get_num_redundant_datas(), 2);
        assert_eq!(stats.get_num_refused_datas(), 1);
        receiver.stats.reset();
        let mut cleared = receiver.stats();
        assert_eq!(cleared.get_num_datas(), 0);
        assert_eq!(
            cleared.get_num_accepted_datas()
                + cleared.get_num_redundant_datas()
                + cleared.get_num_refused_datas(),
            0
        );
    }
    Ok(())
}

#[test]
fn data_ingress_counters_include_saved_reset_data_and_credit_refusal() -> Result<()> {
    let (_, mut receiver) = reset_pair();
    receiver.max_receive_buffer_size = 4096;
    receiver
        .streams
        .get_mut(&1)
        .unwrap()
        .incoming_reset_complete = true;
    let first_tsn = receiver.peer_last_tsn.wrapping_add(1);
    let mut data = ChunkPayloadData {
        tsn: first_tsn,
        stream_identifier: 1,
        beginning_fragment: true,
        ending_fragment: true,
        user_data: Bytes::from_static(b"saved gameplay"),
        ..Default::default()
    };
    receiver.handle_data(&data)?;
    receiver.handle_data(&data)?;
    receiver.max_receive_buffer_size = receiver.deferred_reset_bytes as u32;
    data.tsn = first_tsn.wrapping_add(1);
    receiver.handle_data(&data)?;
    let mut stats = receiver.stats();
    assert_eq!(stats.get_num_datas(), 3);
    assert_eq!(stats.get_num_accepted_datas(), 1);
    assert_eq!(stats.get_num_redundant_datas(), 1);
    assert_eq!(stats.get_num_refused_datas(), 1);
    assert_eq!(receiver.deferred_reset_data.len(), 1);
    Ok(())
}

#[test]
fn data_ingress_acceptance_includes_full_window_completion_and_stopped_drain() -> Result<()> {
    let (_, mut receiver) = reset_pair();
    receiver.max_receive_buffer_size = 1;
    let mut data = ChunkPayloadData {
        tsn: receiver.peer_last_tsn.wrapping_add(1),
        stream_identifier: 1,
        beginning_fragment: true,
        ending_fragment: false,
        user_data: Bytes::from_static(b"part"),
        ..Default::default()
    };
    receiver.handle_data(&data)?;
    assert_eq!(receiver.get_my_receiver_window_credit(), 0);
    data.tsn = data.tsn.wrapping_add(1);
    data.beginning_fragment = false;
    data.ending_fragment = true;
    receiver.handle_data(&data)?;
    assert_eq!(
        receiver
            .stream(1)?
            .read_sctp()?
            .unwrap()
            .to_payload(8)?
            .freeze(),
        Bytes::from_static(b"partpart")
    );
    receiver.set_state(AssociationState::ShutdownPending);
    receiver.streams.get_mut(&1).unwrap().state = RecvSendState::Writable;
    data.tsn = data.tsn.wrapping_add(1);
    data.beginning_fragment = true;
    data.stream_sequence_number = 1;
    receiver.handle_data(&data)?;
    let mut stats = receiver.stats();
    assert_eq!(stats.get_num_datas(), 3);
    assert_eq!(stats.get_num_accepted_datas(), 3);
    assert_eq!(stats.get_num_redundant_datas(), 0);
    assert_eq!(stats.get_num_refused_datas(), 0);
    assert_eq!(receiver.streams[&1].get_num_bytes_in_reassembly_queue(), 0);
    Ok(())
}
