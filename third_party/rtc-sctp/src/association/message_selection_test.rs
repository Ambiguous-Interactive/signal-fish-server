use super::*;

#[test]
fn pending_reset_waits_for_the_fragment_tail() -> Result<()> {
    for unordered in [false, true] {
        let mut a = timed_test_association();
        let now = Instant::now();
        let ppi = PayloadProtocolIdentifier::Binary;
        let fragment_size = a.max_payload_size;
        let mut stream = a.open_stream(1, ppi)?;
        stream.set_reliability_params(unordered, ReliabilityType::Reliable, 0)?;
        stream.write_sctp(
            now,
            &Bytes::from(vec![7; fragment_size as usize * 2 + 1]),
            ppi,
        )?;
        a.send_reset_request(now, 1)?;
        a.cwnd = fragment_size;
        let (head, resets) = a.pop_pending_data_chunks_to_send(now);
        assert_eq!(1, head.len());
        assert!(resets.is_empty());
        assert!(head[0].beginning_fragment && !head[0].ending_fragment);
        assert_eq!(3, a.pending_queue.len()); // two fragments and the marker

        // Window pressure must leave the reset behind the remaining DATA.
        let (data, resets) = a.pop_pending_data_chunks_to_send(now);
        assert!(data.is_empty() && resets.is_empty());
        a.cwnd = u32::MAX;
        let (tail, resets) = a.pop_pending_data_chunks_to_send(now);
        assert_eq!(2, tail.len());
        assert_eq!(vec![1], resets);
        assert!(tail[1].ending_fragment);
        assert_eq!(head[0].tsn.wrapping_add(1), tail[0].tsn);
        assert_eq!(tail[0].tsn.wrapping_add(1), tail[1].tsn);
        assert!(a.pending_queue.is_empty());
        assert_eq!(0, a.pending_queue.get_num_bytes());
    }
    // A control-only reset consumes neither a TSN nor a DATA window slot.
    let mut a = timed_test_association();
    let now = Instant::now();
    let next_tsn = a.my_next_tsn;
    a.cwnd = 0;
    a.rwnd = 0;
    a.send_reset_request(now, 2)?;
    let (data, resets) = a.pop_pending_data_chunks_to_send(now);
    assert!(data.is_empty());
    assert_eq!(vec![2], resets);
    assert_eq!(next_tsn, a.my_next_tsn);
    assert!(a.pending_queue.is_empty());
    Ok(())
}

#[test]
fn invalid_pending_data_cannot_be_sent_or_reset_a_stream() {
    for id in [None, MessageId::new(0)] {
        for empty in [false, true] {
            for zero_window in [false, true] {
                let mut a = timed_test_association();
                let next_tsn = a.my_next_tsn;
                if zero_window {
                    a.cwnd = 0;
                    a.rwnd = 0;
                }
                let rwnd = a.rwnd;
                // Empty DATA is not a reset; a tail without B cannot start a
                // message, including through the zero-window probe path.
                a.pending_queue.push_message(vec![ChunkPayloadData {
                    message_id: id,
                    stream_identifier: 1,
                    beginning_fragment: empty,
                    ending_fragment: true,
                    user_data: if empty {
                        Bytes::new()
                    } else {
                        Bytes::from_static(b"tail")
                    },
                    ..Default::default()
                }]);
                let (data, resets) = a.pop_pending_data_chunks_to_send(Instant::now());
                assert!(data.is_empty() && resets.is_empty());
                assert_eq!(AssociationState::Closed, a.state());
                assert_eq!(next_tsn, a.my_next_tsn);
                assert_eq!(rwnd, a.rwnd);
                assert!(a.poll_timeout().is_none());
            }
        }
    }
}

#[test]
fn message_id_exhaustion_preserves_write_state_and_input() -> Result<()> {
    let mut a = timed_test_association();
    let now = Instant::now();
    let ppi = PayloadProtocolIdentifier::Binary;
    let queued = Bytes::from_static(b"already queued");
    a.next_message_id = u64::MAX - 1;
    a.open_stream(1, ppi)?.write_sctp(now, &queued, ppi)?;
    assert_eq!(u64::MAX, a.next_message_id);
    assert_eq!(
        MessageId::new(u64::MAX - 1),
        a.pending_queue.peek().unwrap().message_id
    );
    let sequence = a.streams.get(&1).unwrap().sequence_number;
    a.stream(1)?
        .set_buffered_amount_high_threshold(queued.len() + 1)?;
    while a.poll().is_some() {}

    let mut input = [Bytes::from_static(b"first"), Bytes::from_static(b"second")];
    let original = input.clone();
    for _ in 0..2 {
        assert!(matches!(
            a.stream(1)?.write_chunks(now, &mut input),
            Err(Error::OtherSctpErr(message)) if message == "message identity exhausted"
        ));
        assert_eq!(original, input, "a failed write must not consume input");
        assert_eq!(u64::MAX, a.next_message_id, "identities must not wrap");
        assert_eq!(sequence, a.streams.get(&1).unwrap().sequence_number);
        assert_eq!(queued.len(), a.stream(1)?.buffered_amount()?);
        assert_eq!(1, a.pending_queue.len());
        assert_eq!(queued.len(), a.pending_queue.get_num_bytes());
        assert_eq!(queued, a.pending_queue.peek().unwrap().user_data);
        assert!(a.inflight_queue.is_empty());
        assert!(
            a.poll().is_none(),
            "a failed write must not emit buffer events"
        );
    }
    // The last successfully allocated identity still carries a sendable message.
    let sent = transmitted_data(&a.gather_outbound(now).0);
    assert_eq!(1, sent.len());
    assert_eq!(queued, sent[0].user_data);
    Ok(())
}

#[test]
fn a_stale_whole_message_selection_cannot_abandon_a_reused_tsn() -> Result<()> {
    for (policy, value) in [(ReliabilityType::Timed, 100), (ReliabilityType::Rexmit, 0)] {
        let first_tsn: u32 = 12345;
        let now = Instant::now();
        let mut sender = timed_test_association();
        let mut receiver = timed_test_association();
        sender.my_next_tsn = first_tsn;
        sender.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        sender.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
        receiver.peer_last_tsn = first_tsn.wrapping_sub(1);
        let ppi = PayloadProtocolIdentifier::Binary;
        let old = Bytes::from_static(b"delivered before the delayed SACK");
        let fresh = Bytes::from_static(b"a new message using the same wire TSN");
        let mut stream = sender.open_stream(1, ppi)?;
        stream.set_reliability_params(false, policy, value)?;
        stream.write_sctp(now, &old, ppi)?;
        let sent = transmitted_data(&sender.gather_outbound(now).0);
        assert_eq!(sent.len(), 1);
        receiver.handle_data(&sent[0])?;
        assert_eq!(
            receiver
                .stream(1)?
                .read_sctp()?
                .unwrap()
                .to_payload(1024)?
                .freeze(),
            old
        );
        let delayed_sack = receiver.create_selective_ack_chunk();

        // The message can be selected for recovery before its real peer
        // acknowledgment arrives, even though the peer already delivered it.
        let retry_at = now + Duration::from_millis(100);
        let candidates =
            sender.unretransmittable_messages(retry_at, ChunkPayloadData::is_outstanding)?;
        assert_eq!(candidates.len(), 1);
        let selected = candidates[0];
        assert!(sender.abandon_message(selected)?);
        assert!(!sender.abandon_message(selected)?);
        sender.handle_sack(&delayed_sack, retry_at)?;
        assert!(sender.inflight_queue.is_empty());
        assert_eq!(sender.stream(1)?.buffered_amount()?, 0);

        // Elide a complete cycle of acknowledged TSNs instead of sending
        // 2^32 packets. Message identity and stream sequence state are not
        // rewound: this is a new message, not the old transport obligation.
        sender.my_next_tsn = first_tsn;
        sender.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        sender.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
        receiver.peer_last_tsn = first_tsn.wrapping_sub(1);
        sender.stream(1)?.write_sctp(retry_at, &fresh, ppi)?;
        let sent = transmitted_data(&sender.gather_outbound(retry_at).0);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].tsn, first_tsn);
        assert_ne!(
            sender.inflight_queue.get(first_tsn).unwrap().message_id,
            Some(selected.id)
        );

        assert!(
            !sender.abandon_message(selected)?,
            "a previously selected TSN must not identify the new message"
        );
        assert_eq!(sender.inflight_queue.get_num_bytes(), fresh.len());
        assert_eq!(sender.stream(1)?.buffered_amount()?, fresh.len());
        receiver.handle_data(&sent[0])?;
        assert_eq!(
            receiver
                .stream(1)?
                .read_sctp()?
                .unwrap()
                .to_payload(1024)?
                .freeze(),
            fresh
        );
        sender.handle_sack(&receiver.create_selective_ack_chunk(), retry_at)?;
        assert!(sender.inflight_queue.is_empty());
        assert_eq!(sender.stream(1)?.buffered_amount()?, 0);
        let mut released = 0;
        while let Some(event) = sender.poll() {
            if let Event::Stream(StreamEvent::BufferedAmountReleased { n_bytes, .. }) = event {
                released += n_bytes;
            }
        }
        assert_eq!(released, old.len() + fresh.len());
    }
    Ok(())
}

#[test]
fn reset_follows_expired_pending_tail_after_partial_ack() -> Result<()> {
    for unordered in [false, true] {
        for ack_mode in 0..3 {
            let mut sender = timed_test_association();
            let now = Instant::now();
            let first_tsn = sender.my_next_tsn;
            let fragment_size = sender.max_payload_size;
            let bytes = fragment_size as usize * 3 + 1;
            let ppi = PayloadProtocolIdentifier::Binary;
            let mut stream = sender.open_stream(1, ppi)?;
            stream.set_reliability_params(unordered, ReliabilityType::Timed, 100)?;
            stream.write_sctp(now, &Bytes::from(vec![9; bytes]), ppi)?;
            stream.close(now)?;
            sender.cwnd = fragment_size * 2;
            let initial = sender.gather_outbound(now).0;
            assert_eq!(2, transmitted_data(&initial).len());
            assert!(sender.reconfigs.is_empty());
            assert_eq!(3, sender.pending_queue.len());
            if ack_mode != 0 {
                sender.handle_sack(
                    &ChunkSelectiveAck {
                        cumulative_tsn_ack: if ack_mode == 1 {
                            first_tsn.wrapping_add(1)
                        } else {
                            first_tsn.wrapping_sub(1)
                        },
                        advertised_receiver_window_credit: 65536,
                        gap_ack_blocks: if ack_mode == 2 {
                            vec![crate::chunk::chunk_selective_ack::GapAckBlock {
                                start: 2,
                                end: 2,
                            }]
                        } else {
                            vec![]
                        },
                        ..Default::default()
                    },
                    now + Duration::from_millis(50),
                )?;
            }
            sender.cwnd = 0;
            sender.rwnd = 0;
            let at = now + Duration::from_millis(100);
            let (packets, keep_open) = sender.gather_outbound(at);
            assert!(keep_open);
            assert_eq!(AssociationState::Established, sender.state());
            assert!(transmitted_data(&packets).is_empty());
            assert!(sender.pending_queue.is_empty());
            assert!(
                sender
                    .stats()
                    .last_abandoned_message()
                    .unwrap()
                    .transmission_selected,
                "already acknowledged prefixes must retain selection evidence"
            );
            assert_eq!(0, sender.pending_queue.get_num_bytes());
            assert_eq!(0, sender.inflight_queue.get_num_bytes());
            assert_eq!(1, sender.reconfigs.len());
            assert_eq!(first_tsn.wrapping_add(4), sender.my_next_tsn);
            let parsed: Vec<Packet> = packets
                .iter()
                .map(Packet::unmarshal)
                .collect::<Result<_>>()?;
            let resets: Vec<_> = parsed
                .iter()
                .flat_map(|p| p.chunks.iter())
                .filter_map(|c| c.as_any().downcast_ref::<ChunkReconfig>())
                .filter_map(|c| c.param_a.as_ref())
                .filter_map(|p| p.as_any().downcast_ref::<ParamOutgoingResetRequest>())
                .collect();
            assert_eq!(1, resets.len());
            assert_eq!(vec![1], resets[0].stream_identifiers);
            assert_eq!(first_tsn.wrapping_add(3), resets[0].sender_last_tsn);
            let forwards: Vec<_> = parsed
                .iter()
                .flat_map(|p| p.chunks.iter())
                .filter_map(|c| c.as_any().downcast_ref::<ChunkForwardTsn>())
                .collect();
            assert_eq!(1, forwards.len());
            assert_eq!(resets[0].sender_last_tsn, forwards[0].new_cumulative_tsn);
            assert_eq!(usize::from(!unordered), forwards[0].streams.len());
            assert!(sender.gather_outbound(at).0.is_empty());
            let released: usize = std::iter::from_fn(|| sender.poll())
                .filter_map(|event| {
                    if let Event::Stream(StreamEvent::BufferedAmountReleased { n_bytes, .. }) =
                        event
                    {
                        Some(n_bytes)
                    } else {
                        None
                    }
                })
                .sum();
            assert_eq!(bytes, released);
        }
    }
    Ok(())
}

// Regression #1095: transport and WebRTC application release credit differ.
#[test]
fn data_channel_credit_tracks_ppid_through_fragment_ack_and_abandonment() -> Result<()> {
    for abandon in [false, true] {
        let now = Instant::now();
        let mut sender = timed_test_association();
        let mut receiver = timed_test_association();
        receiver.peer_last_tsn = sender.my_next_tsn.wrapping_sub(1);
        sender.cwnd = u32::MAX;
        let fragment_size = sender.max_payload_size as usize;
        let text = Bytes::from(vec![b't'; fragment_size * 2 + 17]);
        let binary = Bytes::from(vec![7; fragment_size * 2 + 19]);
        let mut stream = sender.open_stream(1, PayloadProtocolIdentifier::Dcep)?;
        stream.set_reliability_params(false, ReliabilityType::Timed, 100)?;
        for (ppi, bytes) in [
            (
                PayloadProtocolIdentifier::Dcep,
                Bytes::from_static(b"setup-control"),
            ),
            (
                PayloadProtocolIdentifier::StringEmpty,
                Bytes::from_static(&[0]),
            ),
            (
                PayloadProtocolIdentifier::BinaryEmpty,
                Bytes::from_static(&[0]),
            ),
            (PayloadProtocolIdentifier::String, text.clone()),
            (PayloadProtocolIdentifier::Binary, binary.clone()),
        ] {
            stream.write_sctp(now, &bytes, ppi)?;
        }
        if abandon {
            // Send the controls and only the first gameplay fragment. The
            // retirement must release both its in-flight head and queued tail.
            sender.cwnd = (fragment_size + 15) as u32;
        }
        let sent = transmitted_data(&sender.gather_outbound(now).0);
        assert!(
            sent.iter()
                .any(|chunk| chunk.payload_type == PayloadProtocolIdentifier::String)
        );
        let later = now + Duration::from_millis(100);
        let expected_gameplay = if abandon {
            let candidates =
                sender.unretransmittable_messages(later, ChunkPayloadData::is_outstanding)?;
            assert!(!candidates.is_empty());
            for message in candidates {
                assert!(sender.abandon_message(message)?);
                assert!(
                    !sender.abandon_message(message)?,
                    "duplicate retirement releases no bytes"
                );
            }
            text.len() // binary remains entirely pending and was not selected
        } else {
            // Acknowledge gameplay before the older DCEP chunk. This produces
            // a gap SACK and proves overhead cannot be subtracted as a FIFO.
            for chunk in sent
                .iter()
                .filter(|chunk| chunk.payload_type != PayloadProtocolIdentifier::Dcep)
            {
                receiver.handle_data(chunk)?;
            }
            let sack = receiver.create_selective_ack_chunk();
            assert!(!sack.gap_ack_blocks.is_empty());
            sender.handle_sack(&sack, later)?;
            sender.handle_sack(&sack, later)?; // duplicate ACK is not a release
            text.len() + binary.len()
        };
        let mut wire_released = 0;
        let mut gameplay_released = 0;
        let mut buffer_released = 0;
        while let Some(event) = sender.poll() {
            if let Event::Stream(StreamEvent::BufferedAmountReleased {
                n_bytes,
                data_channel_bytes,
                data_channel_buffer_bytes,
                ..
            }) = event
            {
                wire_released += n_bytes;
                gameplay_released += data_channel_bytes;
                buffer_released += data_channel_buffer_bytes;
            }
        }
        assert_eq!(
            gameplay_released, expected_gameplay,
            "abandon={abandon}: exact application credit"
        );
        assert_eq!(
            buffer_released,
            expected_gameplay + 2,
            "abandon={abandon}: empty fillers release their buffer charge once"
        );
        if !abandon {
            assert_eq!(
                wire_released,
                expected_gameplay + 2,
                "empty fillers release transport storage only"
            );
            let control = sent
                .iter()
                .find(|chunk| chunk.payload_type == PayloadProtocolIdentifier::Dcep)
                .unwrap();
            receiver.handle_data(control)?;
            sender.handle_sack(&receiver.create_selective_ack_chunk(), later)?;
            let mut control_credit = 0;
            while let Some(event) = sender.poll() {
                if let Event::Stream(StreamEvent::BufferedAmountReleased {
                    data_channel_bytes,
                    data_channel_buffer_bytes,
                    ..
                }) = event
                {
                    control_credit += data_channel_bytes;
                    assert_eq!(
                        data_channel_buffer_bytes, 0,
                        "DCEP releases no buffer charge"
                    );
                }
            }
            assert_eq!(
                control_credit, 0,
                "late DCEP release cannot credit newer gameplay"
            );
            assert_eq!(sender.stream(1)?.buffered_amount()?, 0);
        } else {
            assert_eq!(
                wire_released,
                expected_gameplay + 2,
                "selected gameplay and empty fillers release transport storage"
            );
            assert_eq!(
                sender.stream(1)?.buffered_amount()?,
                13 + binary.len(),
                "DCEP and unsent binary remain charged to SCTP"
            );
        }
    }
    Ok(())
}

#[test]
fn reassembly_tail_first_gameplay_preserves_exact_payload_and_release() -> Result<()> {
    for unordered in [false, true] {
        for first_tsn in [1000u32, u32::MAX - 512] {
            let now = Instant::now();
            let mut sender = timed_test_association();
            let mut receiver = timed_test_association();
            sender.my_next_tsn = first_tsn;
            sender.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            sender.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            receiver.peer_last_tsn = first_tsn.wrapping_sub(1);
            sender.max_payload_size = 4;
            sender.cwnd = u32::MAX;
            let expected = Bytes::from(
                (0..4096)
                    .map(|index| (index % 251) as u8)
                    .collect::<Vec<_>>(),
            );
            let mut stream = sender.open_stream(1, PayloadProtocolIdentifier::Binary)?;
            stream.set_reliability_params(unordered, ReliabilityType::Reliable, 0)?;
            stream.write_sctp(now, &expected, PayloadProtocolIdentifier::Binary)?;
            let data = transmitted_data(&sender.gather_outbound(now).0);
            assert_eq!(data.len(), 1024);
            let tail = data.last().unwrap();
            receiver.handle_data(tail)?;
            assert!(receiver.stream(1)?.read_sctp()?.is_none());
            for chunk in &data[..data.len() - 1] {
                receiver.handle_data(chunk)?;
            }
            let message = receiver.stream(1)?.read_sctp()?.unwrap();
            assert_eq!(message.to_payload(expected.len())?.freeze(), expected);
            assert!(message.adjacency_checks.get() <= data.len() * 3);
            assert!(receiver.stream(1)?.read_sctp()?.is_none());
            sender.handle_sack(&receiver.create_selective_ack_chunk(), now)?;
            assert!(sender.inflight_queue.is_empty());
            assert_eq!(sender.stream(1)?.buffered_amount()?, 0);
            let released: usize = std::iter::from_fn(|| sender.poll())
                .filter_map(|event| {
                    if let Event::Stream(StreamEvent::BufferedAmountReleased { n_bytes, .. }) =
                        event
                    {
                        Some(n_bytes)
                    } else {
                        None
                    }
                })
                .sum();
            assert_eq!(released, expected.len());
        }
    }
    Ok(())
}

#[test]
fn outbound_evidence_separates_unsent_expiry_retries_and_reliable_delivery() -> Result<()> {
    for unordered in [false, true] {
        for fragmented in [false, true] {
            let now = Instant::now();
            let mut a = timed_test_association();
            let ppi = PayloadProtocolIdentifier::Binary;
            a.max_payload_size = 4;
            a.cwnd = u32::MAX;
            a.rwnd = u32::MAX;
            let body = Bytes::from_static(if fragmented { b"fragmented" } else { b"one" });
            let mut stream = a.open_stream(3, ppi)?;
            stream.set_reliability_params(unordered, ReliabilityType::Timed, 1)?;
            stream.write_sctp(now, &body, ppi)?;
            let queued = a.stats().last_enqueued_message().unwrap();
            assert_eq!(queued.message_id, 0);
            assert_eq!(queued.stream_id, 3);
            assert_eq!(queued.unordered, unordered);
            assert_eq!(queued.reliability, "lifetime");
            let expired_at = now + Duration::from_millis(2);
            let expired_packets = a.gather_outbound(expired_at).0;
            assert!(transmitted_data(&expired_packets).is_empty());
            assert_eq!(a.stats().get_num_emitted_datas(), 0);
            assert_eq!(a.stats().get_num_abandoned_messages(), 1);
            assert_eq!(a.stats().last_abandoned_message(), Some(queued));
            a.gather_outbound(expired_at);
            assert_eq!(a.stats().get_num_abandoned_messages(), 1);

            let mut stream = a.stream(3)?;
            stream.set_reliability_params(unordered, ReliabilityType::Reliable, 0)?;
            stream.write_sctp(expired_at, &body, ppi)?;
            let reliable = a.stats().last_enqueued_message().unwrap();
            assert_eq!(reliable.message_id, 1);
            assert_eq!(reliable.reliability, "reliable");
            let packets = a.gather_outbound(expired_at + Duration::from_secs(1)).0;
            let chunks = transmitted_data(&packets);
            assert_eq!(
                chunks
                    .iter()
                    .flat_map(|c| c.user_data.iter())
                    .copied()
                    .collect::<Vec<_>>(),
                body
            );
            assert_eq!(a.stats().get_num_emitted_datas(), chunks.len() as u64);
            assert_eq!(a.stats().get_num_emitted_retransmissions(), 0);
            assert_eq!(a.stats().get_num_abandoned_messages(), 1);
            assert_eq!(a.stats().last_abandoned_message(), Some(queued));

            for c in &chunks {
                a.inflight_queue.get_mut(c.tsn).unwrap().retransmit = true;
            }
            let mut retry = vec![];
            a.get_data_packets_to_retransmit(expired_at + Duration::from_secs(2), &mut retry)?;
            let retried = transmitted_data(&retry);
            assert_eq!(retried.len(), chunks.len());
            assert_eq!(
                a.stats().get_num_emitted_datas(),
                (chunks.len() + retried.len()) as u64
            );
            assert_eq!(
                a.stats().get_num_emitted_retransmissions(),
                retried.len() as u64
            );
            assert_eq!(a.stats().get_num_abandoned_messages(), 1);
            a.stats.reset();
            assert_eq!(a.stats().get_num_emitted_datas(), 0);
            assert_eq!(a.stats().get_num_emitted_retransmissions(), 0);
            assert_eq!(a.stats().get_num_abandoned_messages(), 0);
            assert_eq!(a.stats().last_enqueued_message(), None);
            assert_eq!(a.stats().last_abandoned_message(), None);
        }
    }
    Ok(())
}

#[test]
fn expired_ordered_prefix_forwards_sequence_and_preserves_exact_reliable_successor() -> Result<()> {
    for fragmented in [false, true] {
        for first_tsn in [1234u32, u32::MAX] {
            let now = Instant::now();
            let mut sender = timed_test_association();
            let mut receiver = timed_test_association();
            sender.my_next_tsn = first_tsn;
            sender.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
            sender.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
            receiver.peer_last_tsn = first_tsn.wrapping_sub(1);
            sender.max_payload_size = 4;
            sender.cwnd = u32::MAX;
            let ppi = PayloadProtocolIdentifier::Binary;
            receiver.open_stream(2, ppi)?;
            let mut stream = sender.open_stream(2, ppi)?;
            stream.set_reliability_params(false, ReliabilityType::Timed, 1)?;
            let expired = Bytes::from_static(if fragmented {
                b"expired fragmented prefix"
            } else {
                b"old"
            });
            stream.write_sctp(now, &expired, ppi)?;
            let later = now + Duration::from_millis(2);
            let skipped = sender.gather_outbound(later).0;
            assert!(
                transmitted_data(&skipped).is_empty(),
                "#1269: expired ordered DATA must not reach the wire"
            );
            assert!(
                skipped
                    .iter()
                    .flat_map(|raw| Packet::unmarshal(raw).unwrap().chunks)
                    .any(|chunk| chunk.as_any().is::<ChunkForwardTsn>())
            );
            for raw in skipped {
                receiver.handle_inbound(Packet::unmarshal(&raw)?, later)?;
            }
            assert!(receiver.stream(2)?.read_sctp()?.is_none());
            for raw in receiver.gather_outbound(later).0 {
                sender.handle_inbound(Packet::unmarshal(&raw)?, later)?;
            }
            assert_eq!(sender.stream(2)?.buffered_amount()?, 0);
            let expected = Bytes::from_static(b"exact reliable successor after expiry");
            sender
                .stream(2)?
                .set_reliability_params(false, ReliabilityType::Reliable, 0)?;
            sender.stream(2)?.write_sctp(later, &expected, ppi)?;
            let sent = sender.gather_outbound(later).0;
            for raw in sent {
                receiver.handle_inbound(Packet::unmarshal(&raw)?, later)?;
            }
            assert_eq!(
                receiver
                    .stream(2)?
                    .read_sctp()?
                    .unwrap()
                    .to_payload(128)?
                    .freeze(),
                expected
            );
            assert!(receiver.stream(2)?.read_sctp()?.is_none());
            assert_eq!(sender.stats().get_num_abandoned_messages(), 1);
            assert_eq!(
                sender.stats().last_abandoned_message().unwrap().message_id,
                0
            );
            assert_eq!(
                sender.stats().last_enqueued_message().unwrap().message_id,
                1
            );
        }
    }
    Ok(())
}

#[test]
fn outbound_evidence_retains_first_abandonment_and_distinguishes_retry_policy() -> Result<()> {
    let now = Instant::now();
    let mut sender = timed_test_association();
    let ppi = PayloadProtocolIdentifier::Binary;
    let mut stream = sender.open_stream(3, ppi)?;
    stream.set_reliability_params(true, ReliabilityType::Timed, 1)?;
    stream.write_sctp(now, &Bytes::from_static(b"expires before send"), ppi)?;
    let first = sender.stats().last_enqueued_message().unwrap();
    let later = now + Duration::from_millis(2);
    sender.gather_outbound(later);
    sender
        .stream(3)?
        .set_reliability_params(true, ReliabilityType::Rexmit, 0)?;
    sender
        .stream(3)?
        .write_sctp(later, &Bytes::from_static(b"no retries"), ppi)?;
    let mut last = sender.stats().last_enqueued_message().unwrap();
    assert!(!first.transmission_selected && !last.transmission_selected);
    assert_eq!(
        first.sequence_number, last.sequence_number,
        "unordered SSN is not message identity"
    );
    assert_ne!(first.message_id, last.message_id);
    sender.gather_outbound(later);
    let retry_at = later + Duration::from_secs(2);
    sender.handle_timeout(retry_at);
    sender.gather_outbound(retry_at);
    assert_eq!(sender.stats().get_num_abandoned_messages(), 2);
    last.transmission_selected = true;
    assert_eq!(sender.stats().first_abandoned_message(), Some(first));
    assert_eq!(sender.stats().last_abandoned_message(), Some(last));
    assert_eq!(last.reliability, "retransmits");
    assert_eq!(sender.stats().get_num_emitted_datas(), 1);
    assert_eq!(sender.stats().get_num_emitted_retransmissions(), 0);
    Ok(())
}

#[test]
fn expired_ordered_backlog_preserves_exact_reliable_and_fixed_lifetime_successors() -> Result<()> {
    for (expired_count, first_tsn, zero_window, lose_first_forward, fixed_lifetime) in [
        (32_768usize, 1234u32, false, false, false),
        (32_770, 1234, false, false, false),
        (65_538, u32::MAX - 3, false, true, false),
        (32_770, u32::MAX - 3, true, true, false),
        (32_770, 1234, false, false, true),
        (65_538, u32::MAX - 3, true, false, true),
    ] {
        let now = Instant::now();
        let mut at = now + Duration::from_millis(2);
        let mut sender = timed_test_association();
        let mut receiver = timed_test_association();
        sender.my_next_tsn = first_tsn;
        sender.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        sender.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
        receiver.peer_last_tsn = first_tsn.wrapping_sub(1);
        receiver.ack_mode = AckMode::NoDelay;
        if zero_window {
            sender.rwnd = 0;
        }
        let ppi = PayloadProtocolIdentifier::Binary;
        receiver.open_stream(2, ppi)?;
        let mut stream = sender.open_stream(2, ppi)?;
        stream.set_reliability_params(false, ReliabilityType::Timed, 1)?;
        for _ in 0..expired_count {
            stream.write_sctp(now, &Bytes::from_static(b"x"), ppi)?;
        }
        if !fixed_lifetime {
            sender
                .stream(2)?
                .set_reliability_params(false, ReliabilityType::Reliable, 0)?;
        }
        let expected = Bytes::from_static(b"exact successor after expired ordered backlog");
        sender.stream(2)?.write_sctp(at, &expected, ppi)?;
        let mut stale_forward = None;
        for round in 0..8 {
            let packets = sender.gather_outbound(at).0;
            if round == 0 && lose_first_forward {
                assert!(
                    transmitted_data(&packets).is_empty(),
                    "even a zero-window probe must wait for sequence progress"
                );
                stale_forward = packets.first().cloned();
                assert!(
                    stale_forward.is_some(),
                    "an actual FORWARD TSN must be lost"
                );
                assert!(sender.gather_outbound(at).0.is_empty());
                at = sender
                    .poll_timeout()
                    .expect("lost FORWARD TSN must retain recovery");
                sender.handle_timeout(at);
                continue;
            }
            for raw in packets {
                receiver.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
            for raw in receiver.gather_outbound(at).0 {
                sender.handle_inbound(Packet::unmarshal(&raw)?, at)?;
            }
            if round == 5
                && let Some(raw) = &stale_forward
            {
                receiver.handle_inbound(Packet::unmarshal(raw)?, at)?;
            }
        }
        let actual = receiver.stream(2)?.read_sctp()?.unwrap_or_else(|| {
            panic!("expired_count={expired_count}, fixed_lifetime={fixed_lifetime}: successor must remain readable")
        });
        assert_eq!(actual.to_payload(expected.len())?.freeze(), expected);
        assert!(receiver.stream(2)?.read_sctp()?.is_none());
        assert_eq!(
            sender.stats().get_num_abandoned_messages(),
            expired_count as u64
        );
        assert_eq!(sender.stream(2)?.buffered_amount()?, 0);
    }
    Ok(())
}

#[test]
fn serial_window_fragmented_expiry_drains_before_reliable_successor() -> Result<()> {
    let now = Instant::now();
    let later = now + Duration::from_millis(2);
    let mut sender = timed_test_association();
    let mut receiver = timed_test_association();
    receiver.peer_last_tsn = sender.my_next_tsn.wrapping_sub(1);
    receiver.ack_mode = AckMode::NoDelay;
    sender.max_payload_size = 4;
    let ppi = PayloadProtocolIdentifier::Binary;
    receiver.open_stream(2, ppi)?;
    let mut stream = sender.open_stream(2, ppi)?;
    stream.set_reliability_params(false, ReliabilityType::Timed, 1)?;
    stream.write_sctp(now, &Bytes::from(vec![0xa5; 4 * 32_770]), ppi)?;
    sender
        .stream(2)?
        .set_reliability_params(false, ReliabilityType::Reliable, 0)?;
    let expected = Bytes::from_static(b"reliable after fragmented expiry");
    sender.stream(2)?.write_sctp(later, &expected, ppi)?;
    let mut forwards = 0;
    for _ in 0..8 {
        for raw in sender.gather_outbound(later).0 {
            let packet = Packet::unmarshal(&raw)?;
            forwards += packet
                .chunks
                .iter()
                .filter(|chunk| chunk.as_any().is::<ChunkForwardTsn>())
                .count();
            receiver.handle_inbound(packet, later)?;
        }
        for raw in receiver.gather_outbound(later).0 {
            sender.handle_inbound(Packet::unmarshal(&raw)?, later)?;
        }
    }
    assert!(
        forwards >= 2,
        "a complete expired message may span multiple forward reports"
    );
    assert_eq!(
        receiver
            .stream(2)?
            .read_sctp()?
            .expect("fragmented expiry must leave successor readable")
            .to_payload(100)?
            .freeze(),
        expected
    );
    assert!(receiver.stream(2)?.read_sctp()?.is_none());
    assert_eq!(sender.stream(2)?.buffered_amount()?, 0);
    assert_eq!(sender.stats().get_num_abandoned_messages(), 1);
    Ok(())
}

#[test]
fn serial_window_live_ordered_data_waits_for_confirmed_progress() -> Result<()> {
    for first_tsn in [1234u32, u32::MAX - 3] {
        let now = Instant::now();
        let mut sender = timed_test_association();
        let mut receiver = timed_test_association();
        sender.my_next_tsn = first_tsn;
        sender.cumulative_tsn_ack_point = first_tsn.wrapping_sub(1);
        sender.advanced_peer_tsn_ack_point = first_tsn.wrapping_sub(1);
        receiver.peer_last_tsn = first_tsn.wrapping_sub(1);
        receiver.ack_mode = AckMode::NoDelay;
        sender.cwnd = u32::MAX;
        sender.rwnd = u32::MAX;
        let ppi = PayloadProtocolIdentifier::Binary;
        receiver.open_stream(2, ppi)?;
        sender.open_stream(2, ppi)?;
        for sequence in 0u32..32_770 {
            sender.stream(2)?.write_sctp(
                now,
                &Bytes::copy_from_slice(&sequence.to_be_bytes()),
                ppi,
            )?;
        }
        let packets = sender.gather_outbound(now).0;
        let mut data = transmitted_data(&packets);
        assert_eq!(
            data.len(),
            32_767,
            "new DATA must leave an unambiguous sequence window"
        );
        sender.rwnd = 0;
        assert!(
            sender.gather_outbound(now).0.is_empty(),
            "window pressure cannot bypass the sequence fence"
        );
        let last = data.pop().unwrap();
        receiver.handle_data(&last)?;
        for chunk in data {
            receiver.handle_data(&chunk)?;
        }
        sender.handle_sack(&receiver.create_selective_ack_chunk(), now)?;
        for raw in sender.gather_outbound(now).0 {
            receiver.handle_inbound(Packet::unmarshal(&raw)?, now)?;
        }
        sender.handle_sack(&receiver.create_selective_ack_chunk(), now)?;
        for sequence in 0u32..32_770 {
            assert_eq!(
                receiver
                    .stream(2)?
                    .read_sctp()?
                    .expect("every reliable sequence must remain readable")
                    .to_payload(4)?
                    .freeze()
                    .as_ref(),
                sequence.to_be_bytes()
            );
        }
        assert!(receiver.stream(2)?.read_sctp()?.is_none());
        assert_eq!(sender.stream(2)?.buffered_amount()?, 0);
    }
    Ok(())
}
