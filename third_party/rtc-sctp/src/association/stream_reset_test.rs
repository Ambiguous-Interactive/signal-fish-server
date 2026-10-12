use super::*;

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
        let old_ack = b.peer_last_tsn;
        b.handle_data(&new_data)?;
        b.handle_data(&new_data)?; // duplicate cannot consume the window twice
        assert_eq!(b.deferred_reset_data.len(), 1);
        assert_eq!(
            b.peer_last_tsn, old_ack,
            "future generation must not be ACKed into old stream"
        );
        assert!(!b.streams.get(&1).unwrap().reassembly_queue.is_readable());
        for p in final_ack {
            for c in p.chunks {
                if let Some(c) = c.as_any().downcast_ref::<ChunkReconfig>() {
                    b.handle_reconfig(c, now)?;
                }
            }
        }
        assert!(b.deferred_reset_data.is_empty());
        assert_eq!(
            b.stream(1)?.read_sctp()?.unwrap().to_payload(100)?.freeze(),
            Bytes::from_static(b"first new generation DATA")
        );
        assert_eq!(b.peer_last_tsn, new_data.tsn);
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
