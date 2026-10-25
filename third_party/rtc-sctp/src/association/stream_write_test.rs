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
    association.streams.get_mut(&1).unwrap().max_payload_size = 1;
    association.max_payload_size = 1;
    assert_eq!(association.stream(1)?.write(Instant::now(), b"ok")?, 2);
    assert_eq!(association.pending_queue.len(), 2);
    assert_eq!(association.streams[&1].sequence_number, 1);
    Ok(())
}

#[test]
fn zero_fragment_budget_public_write_adapters_preserve_inputs() -> Result<()> {
    let mut config = crate::EndpointConfig::default();
    config.max_payload_size(0);
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
            "adapter {adapter}"
        );
        assert_eq!(chunks[0], input);
        assert_eq!(association.next_message_id, identity);
        assert_eq!(association.streams[&1].sequence_number, 0);
        assert!(association.pending_queue.is_empty());
        assert_eq!(association.stream(1)?.buffered_amount()?, 0);
    }
    Ok(())
}
