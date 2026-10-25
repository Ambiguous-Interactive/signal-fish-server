use super::*;
use crate::association::state::AssociationState;

struct UnconsumedSource;

impl BytesSource for UnconsumedSource {
    fn pop_chunk(&mut self, _limit: usize) -> (Bytes, usize) {
        panic!("zero fragmentation budget consumed caller input");
    }

    fn has_remaining(&self) -> bool {
        true
    }
    fn remaining(&self) -> usize {
        1
    }
}

#[test]
fn zero_fragment_budget_rejects_before_consuming_input_or_message_identity() -> Result<()> {
    let mut association = Association::default();
    association.set_state(AssociationState::Established);
    association.max_message_size = 64;
    association.max_payload_size = 0;
    association.open_stream(1, PayloadProtocolIdentifier::Binary)?;
    let identity = association.next_message_id;
    let result = association.stream(1)?.write_source(
        Instant::now(),
        &mut UnconsumedSource,
        PayloadProtocolIdentifier::Binary,
    );
    assert!(matches!(result, Err(Error::ErrOutboundPacketTooLarge)));
    assert_eq!(association.next_message_id, identity);
    assert!(association.pending_queue.is_empty());
    assert_eq!(association.stream(1)?.buffered_amount()?, 0);
    assert_eq!(association.streams[&1].sequence_number, 0);
    // Empty input remains a no-op, even with no fragment capacity.
    assert_eq!(association.stream(1)?.write(Instant::now(), b"")?, 0);
    association.streams.get_mut(&1).unwrap().max_payload_size = 4;
    association.max_payload_size = 4;
    assert_eq!(association.stream(1)?.write(Instant::now(), b"ok")?, 2);
    assert_eq!(association.pending_queue.len(), 1);
    assert_eq!(association.streams[&1].sequence_number, 1);
    Ok(())
}

#[test]
fn zero_fragment_budget_public_write_adapters_preserve_inputs() -> Result<()> {
    for payload_budget in 0..4 {
        let mut config = crate::EndpointConfig::default();
        config.max_payload_size(payload_budget);
        let now = Instant::now();
        let mut association = Association::new(
            None,
            std::sync::Arc::new(crate::TransportConfig::default()),
            config.get_max_payload_size(),
            0,
            "127.0.0.1:5000".parse().unwrap(),
            "127.0.0.1:5001".parse().unwrap(),
            shared::TransportProtocol::UDP,
            now,
        );
        association.set_state(AssociationState::Established);
        association.open_stream(1, PayloadProtocolIdentifier::Binary)?;
        let identity = association.next_message_id;
        let input = Bytes::from_static(b"gameplay");
        for adapter in 0..6 {
            let mut chunks = [input.clone()];
            let mut stream = association.stream(1)?;
            let result = match adapter {
                0 => stream.write(now, &input),
                1 => stream.write_with_ppi(now, &input, PayloadProtocolIdentifier::Binary),
                2 => stream.write_sctp(now, &input, PayloadProtocolIdentifier::Binary),
                3 => stream.write_chunk(now, &input),
                4 => stream.write_chunk_with_ppi(now, &input, PayloadProtocolIdentifier::Binary),
                _ => stream.write_chunks(now, &mut chunks),
            };
            assert!(
                matches!(result, Err(Error::ErrOutboundPacketTooLarge)),
                "payload budget {payload_budget}, adapter {adapter}"
            );
            assert_eq!(chunks[0], input);
            assert_eq!(association.next_message_id, identity);
            assert_eq!(association.streams[&1].sequence_number, 0);
            assert!(association.pending_queue.is_empty());
            assert_eq!(association.stream(1)?.buffered_amount()?, 0);
        }
    }
    Ok(())
}

#[test]
fn chunk_array_applies_message_limit_to_each_write_without_consuming_the_tail() -> Result<()> {
    let mut association = Association::default();
    association.set_state(AssociationState::Established);
    association.max_message_size = 6;
    association.max_payload_size = 4;
    association.open_stream(1, PayloadProtocolIdentifier::Binary)?;
    let now = Instant::now();
    let mut input = [
        Bytes::new(),
        Bytes::from_static(b"first!"),
        Bytes::from_static(b"second"),
    ];
    assert_eq!(association.stream(1)?.write_chunks(now, &mut input)?, 6);
    assert!(input[1].is_empty());
    assert_eq!(input[2], Bytes::from_static(b"second"));
    assert_eq!(association.stream(1)?.write_chunks(now, &mut input)?, 6);
    assert!(input.iter().all(Bytes::is_empty));
    assert_eq!(association.stream(1)?.write_chunks(now, &mut input)?, 0);
    assert_eq!(association.streams[&1].sequence_number, 2);
    assert_eq!(association.stream(1)?.buffered_amount()?, 12);
    let mut bytes = Vec::new();
    let mut messages = Vec::new();
    let mut message = Vec::new();
    while let Some(c) = association.pending_queue.peek() {
        let c = association
            .pending_queue
            .pop(c.beginning_fragment, c.unordered)
            .unwrap();
        bytes.extend_from_slice(&c.user_data);
        assert_eq!(usize::from(c.stream_sequence_number), messages.len());
        assert_eq!(c.beginning_fragment, message.is_empty());
        message.extend_from_slice(&c.user_data);
        if c.ending_fragment {
            messages.push(std::mem::take(&mut message));
        }
    }
    assert_eq!(bytes, b"first!second");
    assert_eq!(messages, [b"first!".to_vec(), b"second".to_vec()]);
    let mut oversized = [Bytes::from_static(b"too big"), Bytes::from_static(b"ok")];
    let original = oversized.clone();
    let identity = association.next_message_id;
    assert!(matches!(
        association.stream(1)?.write_chunks(now, &mut oversized),
        Err(Error::ErrOutboundPacketTooLarge)
    ));
    assert_eq!(oversized, original);
    assert_eq!(association.next_message_id, identity);
    assert_eq!(association.streams[&1].sequence_number, 2);
    Ok(())
}

#[test]
fn packetization_reserves_only_the_required_fragment_slots() {
    for budget in [4, 1200, 65_535] {
        for size in [1, 4, 1201, 65_536] {
            let mut stream = StreamState::new(
                crate::Side::Client,
                1,
                budget,
                PayloadProtocolIdentifier::Binary,
            );
            let payload = Bytes::from(vec![0x5a; size]);
            let (_, chunks) =
                stream.packetize(Instant::now(), &payload, PayloadProtocolIdentifier::Binary);
            let fragment_size = (budget as usize & !3)
                .min(usize::from(u16::MAX) - crate::config::DATA_CHUNK_HEADER_SIZE as usize);
            assert_eq!(chunks.len(), size.div_ceil(fragment_size));
            assert_eq!(
                chunks.capacity(),
                chunks.len(),
                "size={size} budget={budget}"
            );
            let actual: Vec<_> = chunks
                .iter()
                .flat_map(|chunk| chunk.user_data.iter().copied())
                .collect();
            assert_eq!(actual, payload);
            assert!(chunks[0].beginning_fragment);
            assert!(chunks.last().unwrap().ending_fragment);
        }
    }
}

#[test]
#[ignore = "isolated packetization allocation profile"]
fn packetization_allocation_profile() {
    for budget in [4, 1200] {
        let mut stream = StreamState::new(
            crate::Side::Client,
            1,
            budget,
            PayloadProtocolIdentifier::Binary,
        );
        let payload = Bytes::from(vec![0x5a; 65_536]);
        let started = Instant::now();
        let mut slots = 0;
        let mut fragments = 0;
        for _ in 0..1000 {
            let (_, chunks) =
                stream.packetize(Instant::now(), &payload, PayloadProtocolIdentifier::Binary);
            slots += chunks.capacity();
            fragments += chunks.len();
            std::hint::black_box(chunks);
        }
        println!(
            "PACKETIZE budget={budget} fragments={fragments} slots={slots} fragment_bytes={} elapsed_us={}",
            std::mem::size_of::<ChunkPayloadData>(),
            started.elapsed().as_micros()
        );
    }
}
