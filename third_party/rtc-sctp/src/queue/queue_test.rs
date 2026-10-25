use shared::error::{Error, Result};

use bytes::{Bytes, BytesMut};

///////////////////////////////////////////////////////////////////
//payload_queue_test
///////////////////////////////////////////////////////////////////
use super::payload_queue::*;
use crate::chunk::chunk_payload_data::{ChunkPayloadData, PayloadProtocolIdentifier};
use crate::chunk::chunk_selective_ack::GapAckBlock;

fn make_payload(tsn: u32, n_bytes: usize) -> ChunkPayloadData {
    ChunkPayloadData {
        tsn,
        user_data: {
            let mut b = BytesMut::new();
            b.resize(n_bytes, 0);
            b.freeze()
        },
        ..Default::default()
    }
}

#[test]
fn test_payload_queue_push_no_check() -> Result<()> {
    let mut pq = PayloadQueue::new();

    pq.push_no_check(make_payload(0, 10));
    assert_eq!(10, pq.get_num_bytes(), "total bytes mismatch");
    assert_eq!(1, pq.len(), "item count mismatch");
    pq.push_no_check(make_payload(1, 11));
    assert_eq!(21, pq.get_num_bytes(), "total bytes mismatch");
    assert_eq!(2, pq.len(), "item count mismatch");
    pq.push_no_check(make_payload(2, 12));
    assert_eq!(33, pq.get_num_bytes(), "total bytes mismatch");
    assert_eq!(3, pq.len(), "item count mismatch");

    for i in 0..3 {
        assert!(!pq.sorted.is_empty(), "should not be empty");
        let c = pq.pop(i);
        assert!(c.is_some(), "pop should succeed");
        if let Some(c) = c {
            assert_eq!(i, c.tsn, "TSN should match");
        }
    }

    assert_eq!(0, pq.get_num_bytes(), "total bytes mismatch");
    assert_eq!(0, pq.len(), "item count mismatch");

    assert!(pq.sorted.is_empty(), "should be empty");
    pq.push_no_check(make_payload(3, 13));
    assert_eq!(13, pq.get_num_bytes(), "total bytes mismatch");
    pq.push_no_check(make_payload(4, 14));
    assert_eq!(27, pq.get_num_bytes(), "total bytes mismatch");

    for i in 3..5 {
        assert!(!pq.sorted.is_empty(), "should not be empty");
        let c = pq.pop(i);
        assert!(c.is_some(), "pop should succeed");
        if let Some(c) = c {
            assert_eq!(i, c.tsn, "TSN should match");
        }
    }

    assert_eq!(0, pq.get_num_bytes(), "total bytes mismatch");
    assert_eq!(0, pq.len(), "item count mismatch");

    Ok(())
}

#[test]
fn test_payload_queue_get_gap_ack_block() -> Result<()> {
    let mut pq = PayloadQueue::new();

    pq.push(make_payload(1, 0), 0);
    pq.push(make_payload(2, 0), 0);
    pq.push(make_payload(3, 0), 0);
    pq.push(make_payload(4, 0), 0);
    pq.push(make_payload(5, 0), 0);
    pq.push(make_payload(6, 0), 0);

    let gab1 = vec![GapAckBlock { start: 1, end: 6 }];
    let gab2 = pq.get_gap_ack_blocks(0);
    assert!(!gab2.is_empty());
    assert_eq!(gab2.len(), 1);

    assert_eq!(gab1[0].start, gab2[0].start);
    assert_eq!(gab1[0].end, gab2[0].end);

    pq.push(make_payload(8, 0), 0);
    pq.push(make_payload(9, 0), 0);

    let gab1 = vec![
        GapAckBlock { start: 1, end: 6 },
        GapAckBlock { start: 8, end: 9 },
    ];
    let gab2 = pq.get_gap_ack_blocks(0);
    assert!(!gab2.is_empty());
    assert_eq!(gab2.len(), 2);

    assert_eq!(gab1[0].start, gab2[0].start);
    assert_eq!(gab1[0].end, gab2[0].end);
    assert_eq!(gab1[1].start, gab2[1].start);
    assert_eq!(gab1[1].end, gab2[1].end);

    Ok(())
}

#[test]
fn test_payload_queue_get_last_tsn_received() -> Result<()> {
    let mut pq = PayloadQueue::new();

    // empty queie should return false
    let ok = pq.get_last_tsn_received();
    assert!(ok.is_none(), "should be none");

    let ok = pq.push(make_payload(20, 0), 0);
    assert!(ok, "should be true");
    let tsn = pq.get_last_tsn_received();
    assert!(tsn.is_some(), "should be false");
    assert_eq!(Some(&20), tsn, "should match");

    // append should work
    let ok = pq.push(make_payload(21, 0), 0);
    assert!(ok, "should be true");
    let tsn = pq.get_last_tsn_received();
    assert!(tsn.is_some(), "should be false");
    assert_eq!(Some(&21), tsn, "should match");

    // check if sorting applied
    let ok = pq.push(make_payload(19, 0), 0);
    assert!(ok, "should be true");
    let tsn = pq.get_last_tsn_received();
    assert!(tsn.is_some(), "should be false");
    assert_eq!(Some(&21), tsn, "should match");

    Ok(())
}

#[test]
fn test_payload_queue_mark_all_to_retrasmit() -> Result<()> {
    let mut pq = PayloadQueue::new();

    for i in 0..3 {
        pq.push(make_payload(i + 1, 10), 0);
    }
    pq.acknowledge(2);
    pq.mark_all_to_retrasmit();

    let c = pq.get(1);
    assert!(c.is_some(), "should be true");
    assert!(c.unwrap().retransmit, "should be marked as retransmit");
    let c = pq.get(2);
    assert!(c.is_some(), "should be true");
    assert!(!c.unwrap().retransmit, "should NOT be marked as retransmit");
    let c = pq.get(3);
    assert!(c.is_some(), "should be true");
    assert!(c.unwrap().retransmit, "should be marked as retransmit");

    Ok(())
}

#[test]
fn test_payload_queue_reset_retransmit_flag_on_ack() -> Result<()> {
    let mut pq = PayloadQueue::new();

    for i in 0..4 {
        pq.push(make_payload(i + 1, 10), 0);
    }

    pq.mark_all_to_retrasmit();
    pq.acknowledge(2); // should cancel retransmission for TSN 2
    pq.acknowledge(4); // should cancel retransmission for TSN 4

    let c = pq.get(1);
    assert!(c.is_some(), "should be true");
    assert!(c.unwrap().retransmit, "should be marked as retransmit");
    let c = pq.get(2);
    assert!(c.is_some(), "should be true");
    assert!(!c.unwrap().retransmit, "should NOT be marked as retransmit");
    let c = pq.get(3);
    assert!(c.is_some(), "should be true");
    assert!(c.unwrap().retransmit, "should be marked as retransmit");
    let c = pq.get(4);
    assert!(c.is_some(), "should be true");
    assert!(!c.unwrap().retransmit, "should NOT be marked as retransmit");

    Ok(())
}

///////////////////////////////////////////////////////////////////
//pending_queue_test
///////////////////////////////////////////////////////////////////
use super::pending_queue::*;

const NO_FRAGMENT: usize = 0;
const FRAG_BEGIN: usize = 1;
const FRAG_MIDDLE: usize = 2;
const FRAG_END: usize = 3;

fn make_data_chunk(tsn: u32, unordered: bool, frag: usize) -> ChunkPayloadData {
    let mut b = false;
    let mut e = false;

    match frag {
        NO_FRAGMENT => {
            b = true;
            e = true;
        }
        FRAG_BEGIN => {
            b = true;
        }
        FRAG_END => e = true,
        _ => {}
    };

    ChunkPayloadData {
        tsn,
        unordered,
        beginning_fragment: b,
        ending_fragment: e,
        user_data: {
            let mut b = BytesMut::new();
            b.resize(10, 0); // always 10 bytes
            b.freeze()
        },
        ..Default::default()
    }
}

#[test]
fn test_pending_base_queue_push_and_pop() -> Result<()> {
    let mut pq = PendingBaseQueue::new();
    pq.push_back(make_data_chunk(0, false, NO_FRAGMENT));
    pq.push_back(make_data_chunk(1, false, NO_FRAGMENT));
    pq.push_back(make_data_chunk(2, false, NO_FRAGMENT));

    for i in 0..3 {
        let c = pq.get(i);
        assert!(c.is_some(), "should not be none");
        assert_eq!(i as u32, c.unwrap().tsn, "TSN should match");
    }

    for i in 0..3 {
        let c = pq.pop_front();
        assert!(c.is_some(), "should not be none");
        assert_eq!(i, c.unwrap().tsn, "TSN should match");
    }

    pq.push_back(make_data_chunk(3, false, NO_FRAGMENT));
    pq.push_back(make_data_chunk(4, false, NO_FRAGMENT));

    for i in 3..5 {
        let c = pq.pop_front();
        assert!(c.is_some(), "should not be none");
        assert_eq!(i, c.unwrap().tsn, "TSN should match");
    }
    Ok(())
}

#[test]
fn test_pending_base_queue_out_of_bounce() -> Result<()> {
    let mut pq = PendingBaseQueue::new();
    assert!(pq.pop_front().is_none(), "should be none");
    assert!(pq.get(0).is_none(), "should be none");

    pq.push_back(make_data_chunk(0, false, NO_FRAGMENT));
    assert!(pq.get(1).is_none(), "should be none");

    Ok(())
}

// NOTE: TSN is not used in pendingQueue in the actual usage.
//       Following tests use TSN field as a chunk ID.
#[test]
fn test_pending_queue_push_and_pop() -> Result<()> {
    let mut pq = PendingQueue::new();
    pq.push_message(vec![make_data_chunk(0, false, NO_FRAGMENT)]);
    assert_eq!(10, pq.get_num_bytes(), "total bytes mismatch");
    pq.push_message(vec![make_data_chunk(1, false, NO_FRAGMENT)]);
    assert_eq!(20, pq.get_num_bytes(), "total bytes mismatch");
    pq.push_message(vec![make_data_chunk(2, false, NO_FRAGMENT)]);
    assert_eq!(30, pq.get_num_bytes(), "total bytes mismatch");

    for i in 0..3 {
        let c = pq.peek();
        assert!(c.is_some(), "peek error");
        let c = c.unwrap();
        assert_eq!(i, c.tsn, "TSN should match");
        let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);

        let result = pq.pop(beginning_fragment, unordered);
        assert!(result.is_some(), "should not error: {}", i);
    }

    assert_eq!(0, pq.get_num_bytes(), "total bytes mismatch");

    pq.push_message(vec![make_data_chunk(3, false, NO_FRAGMENT)]);
    assert_eq!(10, pq.get_num_bytes(), "total bytes mismatch");
    pq.push_message(vec![make_data_chunk(4, false, NO_FRAGMENT)]);
    assert_eq!(20, pq.get_num_bytes(), "total bytes mismatch");

    for i in 3..5 {
        let c = pq.peek();
        assert!(c.is_some(), "peek error");
        let c = c.unwrap();
        assert_eq!(i, c.tsn, "TSN should match");
        let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);

        let result = pq.pop(beginning_fragment, unordered);
        assert!(result.is_some(), "should not error: {}", i);
    }

    assert_eq!(0, pq.get_num_bytes(), "total bytes mismatch");

    Ok(())
}

#[test]
fn test_pending_queue_unordered_wins() -> Result<()> {
    let mut pq = PendingQueue::new();

    pq.push_message(vec![make_data_chunk(0, false, NO_FRAGMENT)]);
    assert_eq!(10, pq.get_num_bytes(), "total bytes mismatch");
    pq.push_message(vec![make_data_chunk(1, true, NO_FRAGMENT)]);
    assert_eq!(20, pq.get_num_bytes(), "total bytes mismatch");
    pq.push_message(vec![make_data_chunk(2, false, NO_FRAGMENT)]);
    assert_eq!(30, pq.get_num_bytes(), "total bytes mismatch");
    pq.push_message(vec![make_data_chunk(3, true, NO_FRAGMENT)]);
    assert_eq!(40, pq.get_num_bytes(), "total bytes mismatch");

    let c = pq.peek();
    assert!(c.is_some(), "peek error");
    let c = c.unwrap();
    assert_eq!(1, c.tsn, "TSN should match");
    let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);
    let result = pq.pop(beginning_fragment, unordered);
    assert!(result.is_some(), "should not error");

    let c = pq.peek();
    assert!(c.is_some(), "peek error");
    let c = c.unwrap();
    assert_eq!(3, c.tsn, "TSN should match");
    let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);
    let result = pq.pop(beginning_fragment, unordered);
    assert!(result.is_some(), "should not error");

    let c = pq.peek();
    assert!(c.is_some(), "peek error");
    let c = c.unwrap();
    assert_eq!(0, c.tsn, "TSN should match");
    let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);
    let result = pq.pop(beginning_fragment, unordered);
    assert!(result.is_some(), "should not error");

    let c = pq.peek();
    assert!(c.is_some(), "peek error");
    let c = c.unwrap();
    assert_eq!(2, c.tsn, "TSN should match");
    let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);
    let result = pq.pop(beginning_fragment, unordered);
    assert!(result.is_some(), "should not error");

    assert_eq!(0, pq.get_num_bytes(), "total bytes mismatch");

    Ok(())
}

#[test]
fn test_pending_queue_fragments() -> Result<()> {
    let mut pq = PendingQueue::new();
    pq.push_message(vec![
        make_data_chunk(0, false, FRAG_BEGIN),
        make_data_chunk(1, false, FRAG_MIDDLE),
        make_data_chunk(2, false, FRAG_END),
    ]);
    pq.push_message(vec![
        make_data_chunk(3, true, FRAG_BEGIN),
        make_data_chunk(4, true, FRAG_MIDDLE),
        make_data_chunk(5, true, FRAG_END),
    ]);

    let expects = vec![3, 4, 5, 0, 1, 2];

    for exp in expects {
        let c = pq.peek();
        assert!(c.is_some(), "peek error");
        let c = c.unwrap();
        assert_eq!(exp, c.tsn, "TSN should match");
        let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);
        let result = pq.pop(beginning_fragment, unordered);
        assert!(result.is_some(), "should not error: {}", exp);
    }

    Ok(())
}

// Once decided ordered or unordered, the decision should persist until
// it pops a chunk with ending_fragment flags set to true.
#[test]
fn test_pending_queue_selection_persistence() -> Result<()> {
    let mut pq = PendingQueue::new();
    pq.push_message(vec![
        make_data_chunk(0, false, FRAG_BEGIN),
        make_data_chunk(2, false, FRAG_MIDDLE),
        make_data_chunk(3, false, FRAG_END),
    ]);

    let c = pq.peek();
    assert!(c.is_some(), "peek error");
    let c = c.unwrap();
    assert_eq!(0, c.tsn, "TSN should match");
    let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);
    let result = pq.pop(beginning_fragment, unordered);
    assert!(result.is_some(), "should not error: {}", 0);

    pq.push_message(vec![make_data_chunk(1, true, NO_FRAGMENT)]);

    let expects = vec![2, 3, 1];

    for exp in expects {
        let c = pq.peek();
        assert!(c.is_some(), "peek error");
        let c = c.unwrap();
        assert_eq!(exp, c.tsn, "TSN should match");
        let (beginning_fragment, unordered) = (c.beginning_fragment, c.unordered);
        let result = pq.pop(beginning_fragment, unordered);
        assert!(result.is_some(), "should not error: {}", exp);
    }

    Ok(())
}

///////////////////////////////////////////////////////////////////
//reassembly_queue_test
///////////////////////////////////////////////////////////////////
use super::reassembly_queue::*;

#[test]
fn test_reassembly_queue_ordered_fragments() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        tsn: 1,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(3, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        ending_fragment: true,
        tsn: 2,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"DEFG"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(complete, "chunk set should be complete");
    assert_eq!(7, rq.get_num_bytes(), "num bytes mismatch");

    let mut buf = vec![0u8; 16];

    if let Some(chunks) = rq.read() {
        let n = chunks.read(&mut buf)?;
        assert_eq!(7, n, "should received 7 bytes");
        assert_eq!(0, rq.get_num_bytes(), "num bytes mismatch");
        assert_eq!(chunks.ppi, org_ppi, "should have valid ppi");
        assert_eq!(&buf[..n], b"ABCDEFG", "data should match");
    } else {
        assert!(false);
    }

    Ok(())
}

#[test]
fn test_reassembly_queue_to_payload() -> Result<()> {
    // A multi-fragment ordered message: "ABC" + "DEFG" == "ABCDEFG" (7 bytes).
    let mut rq = ReassemblyQueue::new(0);
    let org_ppi = PayloadProtocolIdentifier::Binary;

    rq.push(ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        tsn: 1,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    });
    let complete = rq.push(ChunkPayloadData {
        payload_type: org_ppi,
        ending_fragment: true,
        tsn: 2,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"DEFG"),
        ..Default::default()
    });
    assert!(complete, "chunk set should be complete");

    let chunks = rq.read().expect("a complete message");

    // Ample buffer: all fragments concatenated in order with a single copy.
    let payload = chunks.to_payload(16)?;
    assert_eq!(
        &payload[..],
        b"ABCDEFG",
        "to_payload must concatenate fragments in order"
    );

    // Exact boundary (max_len == total) still succeeds.
    assert_eq!(
        &chunks.to_payload(7)?[..],
        b"ABCDEFG",
        "max_len == total must succeed"
    );

    // Too small (max_len < total) is rejected with ErrShortBuffer, matching read().
    assert!(
        matches!(chunks.to_payload(6), Err(Error::ErrShortBuffer)),
        "to_payload must reject oversized messages like read()"
    );

    Ok(())
}

#[test]
fn test_reassembly_queue_unordered_fragments() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        beginning_fragment: true,
        tsn: 1,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(3, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        tsn: 2,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"DEFG"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(7, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        ending_fragment: true,
        tsn: 3,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"H"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(complete, "chunk set should be complete");
    assert_eq!(8, rq.get_num_bytes(), "num bytes mismatch");

    let mut buf = vec![0u8; 16];

    if let Some(chunks) = rq.read() {
        let n = chunks.read(&mut buf)?;
        assert_eq!(8, n, "should received 8 bytes");
        assert_eq!(0, rq.get_num_bytes(), "num bytes mismatch");
        assert_eq!(chunks.ppi, org_ppi, "should have valid ppi");
        assert_eq!(&buf[..n], b"ABCDEFGH", "data should match");
    } else {
        assert!(false);
    }

    Ok(())
}

/*TODO: reassembly_queue is changed by introducing timestamp for unordered and ordered chunks
#[test]
fn test_reassembly_queue_ordered_and_unordered_fragments() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);
    let org_ppi = PayloadProtocolIdentifier::Binary;
    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 1,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(complete, "chunk set should be complete");
    assert_eq!(3, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 2,
        stream_sequence_number: 1,
        user_data: Bytes::from_static(b"DEF"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(complete, "chunk set should be complete");
    assert_eq!(6, rq.get_num_bytes(), "num bytes mismatch");

    //
    // Now we have two complete chunks ready to read in the reassemblyQueue.
    //

    let mut buf = vec![0u8; 16];

    // Should read unordered chunks first
    if let Some(chunks) = rq.read() {
        let n = chunks.read(&mut buf)?;
        assert_eq!(3, n, "should received 3 bytes");
        assert_eq!(3, rq.get_num_bytes(), "num bytes mismatch");
        assert_eq!(chunks.ppi, org_ppi, "should have valid ppi");
        assert_eq!(&buf[..n], b"DEF", "data should match");
    } else {
        assert!(false);
    }

    // Next should read ordered chunks
    if let Some(chunks) = rq.read() {
        let n = chunks.read(&mut buf)?;
        assert_eq!(3, n, "should received 3 bytes");
        assert_eq!(0, rq.get_num_bytes(), "num bytes mismatch");
        assert_eq!(chunks.ppi, org_ppi, "should have valid ppi");
        assert_eq!(&buf[..n], b"ABC", "data should match");
    } else {
        assert!(false);
    }

    Ok(())
}
*/

/// When both an unordered and an ordered message are readable, `read`
/// returns the one that completed first (timestamp order).
#[test]
fn test_reassembly_queue_read_prefers_earlier_completion() -> Result<()> {
    let org_ppi = PayloadProtocolIdentifier::Binary;
    let make_chunk = |tsn: u32, ssn: u16, unordered: bool, data: &'static [u8]| ChunkPayloadData {
        payload_type: org_ppi,
        unordered,
        beginning_fragment: true,
        ending_fragment: true,
        tsn,
        stream_sequence_number: ssn,
        user_data: Bytes::from_static(data),
        ..Default::default()
    };
    let mut buf = vec![0u8; 16];

    // Ordered completes first -> read it first, unordered second.
    let mut rq = ReassemblyQueue::new(0);
    // No sleep between the pushes: `Chunks::arrival` is a monotonic counter incremented per
    // completed message, not a timestamp, so ordering follows insertion order. The sleep this
    // replaces dated from when `arrival` was an `Instant` and bought nothing but 2ms.
    assert!(rq.push(make_chunk(1, 0, false, b"ABC")));
    assert!(rq.push(make_chunk(2, 1, true, b"DEF")));

    let chunks = rq.read().expect("first read");
    assert_eq!(3, chunks.read(&mut buf)?);
    assert_eq!(&buf[..3], b"ABC", "older (ordered) message should win");
    let chunks = rq.read().expect("second read");
    assert_eq!(3, chunks.read(&mut buf)?);
    assert_eq!(&buf[..3], b"DEF");
    assert_eq!(0, rq.get_num_bytes(), "num bytes mismatch");

    // Unordered completes first -> read it first, ordered second.
    let mut rq = ReassemblyQueue::new(0);
    assert!(rq.push(make_chunk(2, 1, true, b"DEF")));
    assert!(rq.push(make_chunk(1, 0, false, b"ABC")));

    let chunks = rq.read().expect("first read");
    assert_eq!(3, chunks.read(&mut buf)?);
    assert_eq!(&buf[..3], b"DEF", "older (unordered) message should win");
    let chunks = rq.read().expect("second read");
    assert_eq!(3, chunks.read(&mut buf)?);
    assert_eq!(&buf[..3], b"ABC");
    assert_eq!(0, rq.get_num_bytes(), "num bytes mismatch");

    Ok(())
}

#[test]
fn test_reassembly_queue_unordered_complete_skips_incomplete() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        beginning_fragment: true,
        tsn: 10,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"IN"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(2, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        ending_fragment: true,
        tsn: 12, // <- incongiguous
        stream_sequence_number: 1,
        user_data: Bytes::from_static(b"COMPLETE"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(10, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 13,
        stream_sequence_number: 1,
        user_data: Bytes::from_static(b"GOOD"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(complete, "chunk set should be complete");
    assert_eq!(14, rq.get_num_bytes(), "num bytes mismatch");

    //
    // Now we have two complete chunks ready to read in the reassemblyQueue.
    //

    let mut buf = vec![0u8; 16];

    // Should pick the one that has "GOOD"
    if let Some(chunks) = rq.read() {
        let n = chunks.read(&mut buf)?;
        assert_eq!(4, n, "should receive 4 bytes");
        assert_eq!(10, rq.get_num_bytes(), "num bytes mismatch");
        assert_eq!(chunks.ppi, org_ppi, "should have valid ppi");
        assert_eq!(&buf[..n], b"GOOD", "data should match");
    } else {
        assert!(false);
    }

    Ok(())
}

#[test]
fn test_reassembly_queue_ignores_chunk_with_wrong_si() -> Result<()> {
    let mut rq = ReassemblyQueue::new(123);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        stream_identifier: 124,
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 10,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"IN"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk should be ignored");
    assert_eq!(0, rq.get_num_bytes(), "num bytes mismatch");
    Ok(())
}

#[test]
fn test_reassembly_queue_ignores_chunk_with_stale_ssn() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);
    rq.next_ssn = 7; // forcibly set expected SSN to 7

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 10,
        stream_sequence_number: 6, // <-- stale
        user_data: Bytes::from_static(b"IN"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk should not be ignored");
    assert_eq!(0, rq.get_num_bytes(), "num bytes mismatch");

    Ok(())
}

#[test]
fn test_reassembly_queue_should_fail_to_read_incomplete_chunk() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        tsn: 123,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"IN"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "the set should not be complete");
    assert_eq!(2, rq.get_num_bytes(), "num bytes mismatch");

    let result = rq.read();
    assert!(result.is_none(), "read() should not succeed");
    assert_eq!(2, rq.get_num_bytes(), "num bytes mismatch");

    Ok(())
}

#[test]
fn test_reassembly_queue_should_fail_to_read_if_the_nex_ssn_is_not_ready() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 123,
        stream_sequence_number: 1,
        user_data: Bytes::from_static(b"IN"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(complete, "the set should be complete");
    assert_eq!(2, rq.get_num_bytes(), "num bytes mismatch");

    let result = rq.read();
    assert!(result.is_none(), "read() should not succeed");
    assert_eq!(2, rq.get_num_bytes(), "num bytes mismatch");

    Ok(())
}

#[test]
fn test_reassembly_queue_detect_buffer_too_short() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 123,
        stream_sequence_number: 0,
        user_data: Bytes::from_static(b"0123456789"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(complete, "the set should be complete");
    assert_eq!(10, rq.get_num_bytes(), "num bytes mismatch");

    let mut buf = vec![0u8; 8]; // <- passing buffer too short
    if let Some(chunks) = rq.read() {
        let result = chunks.read(&mut buf);
        assert!(result.is_err(), "read() should not succeed");
        if let Err(err) = result {
            assert_eq!(Error::ErrShortBuffer, err, "read() should not succeed");
        }
        assert_eq!(0, rq.get_num_bytes(), "num bytes mismatch");
    } else {
        assert!(false);
    }

    Ok(())
}

#[test]
fn test_reassembly_queue_forward_tsn_for_ordered_framents() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let ssn_complete = 5u16;
    let ssn_dropped = 6u16;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        ending_fragment: true,
        tsn: 10,
        stream_sequence_number: ssn_complete,
        user_data: Bytes::from_static(b"123"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(complete, "chunk set should be complete");
    assert_eq!(3, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        beginning_fragment: true,
        tsn: 11,
        stream_sequence_number: ssn_dropped,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(6, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        tsn: 12,
        stream_sequence_number: ssn_dropped,
        user_data: Bytes::from_static(b"DEF"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(9, rq.get_num_bytes(), "num bytes mismatch");

    rq.forward_tsn_for_ordered(ssn_dropped);

    assert_eq!(3, rq.get_num_bytes(), "num bytes mismatch");
    assert_eq!(rq.read().unwrap().to_payload(3)?.as_ref(), b"123");
    assert_eq!(
        0,
        rq.get_num_bytes(),
        "read must release the complete message"
    );

    Ok(())
}

#[test]
fn test_reassembly_queue_forward_tsn_for_unordered_framents() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);

    let org_ppi = PayloadProtocolIdentifier::Binary;

    let ssn_dropped = 6u16;
    let ssn_kept = 7u16;

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        beginning_fragment: true,
        tsn: 11,
        stream_sequence_number: ssn_dropped,
        user_data: Bytes::from_static(b"ABC"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(3, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        tsn: 12,
        stream_sequence_number: ssn_dropped,
        user_data: Bytes::from_static(b"DEF"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(6, rq.get_num_bytes(), "num bytes mismatch");

    let chunk = ChunkPayloadData {
        payload_type: org_ppi,
        unordered: true,
        tsn: 14,
        beginning_fragment: true,
        stream_sequence_number: ssn_kept,
        user_data: Bytes::from_static(b"SOS"),
        ..Default::default()
    };

    let complete = rq.push(chunk);
    assert!(!complete, "chunk set should not be complete yet");
    assert_eq!(9, rq.get_num_bytes(), "num bytes mismatch");

    // At this point, there are 3 chunks in the rq.unorderedChunks.
    // This call should remove chunks with tsn equals to 13 or older.
    rq.forward_tsn_for_unordered(13);

    // As a result, there should be one chunk (tsn=14)
    assert_eq!(
        1,
        rq.unordered_chunks.len(),
        "there should be one chunk kept"
    );
    assert_eq!(3, rq.get_num_bytes(), "num bytes mismatch");

    Ok(())
}

#[test]
fn test_chunk_set_empty_chunk_set() -> Result<()> {
    let cset = Chunks::new(0, 0, PayloadProtocolIdentifier::default(), vec![]);
    assert!(!cset.is_complete(), "empty chunkSet cannot be complete");
    Ok(())
}

#[test]
fn test_chunk_set_push_dup_chunks_to_chunk_set() -> Result<()> {
    let mut cset = Chunks::new(0, 0, PayloadProtocolIdentifier::default(), vec![]);
    cset.push(ChunkPayloadData {
        tsn: 100,
        beginning_fragment: true,
        ..Default::default()
    });
    let complete = cset.push(ChunkPayloadData {
        tsn: 100,
        ending_fragment: true,
        ..Default::default()
    });
    assert!(!complete, "chunk with dup TSN is not complete");
    assert_eq!(1, cset.chunks.len(), "chunk with dup TSN should be ignored");
    Ok(())
}

#[test]
fn test_chunk_set_incomplete_chunk_set_no_beginning() -> Result<()> {
    let cset = Chunks::new(0, 0, PayloadProtocolIdentifier::default(), vec![]);
    assert!(
        !cset.is_complete(),
        "chunkSet not starting with B=1 cannot be complete"
    );
    Ok(())
}

#[test]
fn test_chunk_set_incomplete_chunk_set_no_contiguous_tsn() -> Result<()> {
    let cset = Chunks::new(
        0,
        0,
        PayloadProtocolIdentifier::default(),
        vec![
            ChunkPayloadData {
                tsn: 100,
                beginning_fragment: true,
                ..Default::default()
            },
            ChunkPayloadData {
                tsn: 101,
                ..Default::default()
            },
            ChunkPayloadData {
                tsn: 103,
                ending_fragment: true,
                ..Default::default()
            },
        ],
    );
    assert!(
        !cset.is_complete(),
        "chunkSet not starting with incontiguous tsn cannot be complete"
    );
    Ok(())
}

#[test]
fn test_reassembly_queue_ssn_overflow() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);
    let org_ppi = PayloadProtocolIdentifier::Binary;

    for stream_sequence_number in 0..=u16::MAX {
        let chunk = ChunkPayloadData {
            payload_type: org_ppi,
            beginning_fragment: true,
            ending_fragment: true,
            tsn: 10,
            stream_sequence_number,
            user_data: Bytes::from_static(b"123"),
            ..Default::default()
        };
        assert!(rq.push(chunk));
        assert!(rq.read().is_some());
    }

    Ok(())
}

#[test]
fn test_reassembly_queue_ssn_overflow_in_forward_tsn_for_ordered() -> Result<()> {
    let mut rq = ReassemblyQueue::new(0);
    let org_ppi = PayloadProtocolIdentifier::Binary;

    for stream_sequence_number in 0..u16::MAX {
        let chunk = ChunkPayloadData {
            payload_type: org_ppi,
            beginning_fragment: true,
            ending_fragment: true,
            tsn: 10,
            stream_sequence_number,
            user_data: Bytes::from_static(b"123"),
            ..Default::default()
        };
        assert!(rq.push(chunk));
        assert!(rq.read().is_some());
    }
    rq.forward_tsn_for_ordered(u16::MAX);

    Ok(())
}

#[test]
fn test_message_index_survives_cumulative_prefix_and_tsn_wrap() {
    use crate::chunk::chunk_payload_data::{MessageId, MessageReliability};
    let mut queue = PayloadQueue::new();
    for (tsn, id, beginning, ending) in [
        (0, 11, false, true),
        (u32::MAX, 11, true, false),
        // A lone remaining tail still belongs to a fragmented message.
        (1, 12, false, true),
    ] {
        let mut chunk = make_payload(tsn, 10);
        chunk.message_id = MessageId::new(id);
        chunk.reliability = MessageReliability::Rexmit { max_retransmits: 1 };
        chunk.beginning_fragment = beginning;
        chunk.ending_fragment = ending;
        queue.push_no_check(chunk);
    }
    assert_eq!(
        vec![u32::MAX, 0],
        queue.message_tsns(MessageId::new(11).unwrap())
    );
    queue.pop(u32::MAX).unwrap();
    assert_eq!(vec![0], queue.message_tsns(MessageId::new(11).unwrap()));
    queue.pop(0).unwrap();
    assert!(queue.message_tsns(MessageId::new(11).unwrap()).is_empty());
    assert_eq!(vec![1], queue.message_tsns(MessageId::new(12).unwrap()));
}

// Issue #1158: gap offsets are serial distances, including across TSN wrap.
#[test]
fn test_payload_queue_gap_ack_preserves_serial_offsets() {
    for cumulative in [0u32, u32::MAX - 2, u32::MAX] {
        let mut queue = PayloadQueue::new();
        for offset in [5u32, 2, 4, 3] {
            assert!(queue.push(make_payload(cumulative.wrapping_add(offset), 1), cumulative));
        }
        let gaps = queue.get_gap_ack_blocks(cumulative);
        assert_eq!(gaps.len(), 1, "cumulative={cumulative}");
        assert_eq!((gaps[0].start, gaps[0].end), (2, 5));
        assert_eq!(queue.len(), 4);
        assert_eq!(queue.get_num_bytes(), 4);
    }
}

#[test]
fn test_payload_queue_gap_ack_omits_unrepresentable_offsets() {
    for cumulative in [0u32, u32::MAX - 2] {
        let mut queue = PayloadQueue::new();
        for offset in [65534u32, 65535, 65536, 65538] {
            assert!(queue.push(make_payload(cumulative.wrapping_add(offset), 1), cumulative));
        }
        let gaps = queue.get_gap_ack_blocks(cumulative);
        assert_eq!(gaps.len(), 1, "cumulative={cumulative}");
        assert_eq!((gaps[0].start, gaps[0].end), (65534, 65535));
        assert_eq!(queue.len(), 4, "unreported DATA remains retained");
        let advanced = cumulative.wrapping_add(65533);
        let gaps = queue.get_gap_ack_blocks(advanced);
        assert_eq!(gaps.len(), 2);
        assert_eq!((gaps[0].start, gaps[0].end), (1, 3));
        assert_eq!((gaps[1].start, gaps[1].end), (5, 5));

        let mut far = PayloadQueue::new();
        assert!(far.push(make_payload(cumulative.wrapping_add(65536), 1), cumulative));
        assert!(far.get_gap_ack_blocks(cumulative).is_empty());
        assert_eq!(far.len(), 1);
    }
}

#[test]
fn test_payload_queue_gap_ack_ordered_insert_work_and_reordering() {
    for first in [1_u32, u32::MAX - 512] {
        let mut queue = PayloadQueue::new();
        for offset in 0..1024 {
            queue.push_no_check(make_payload(first.wrapping_add(offset), 1));
        }
        assert!(
            queue.ordering_comparisons.get() <= 1024,
            "ordered insertion made {} comparisons",
            queue.ordering_comparisons.get()
        );
        for offset in 0..512 {
            assert_eq!(
                queue.pop(first.wrapping_add(offset)).unwrap().tsn,
                first.wrapping_add(offset)
            );
        }
        // Force a wrapped VecDeque and out-of-order insertion behind its tail.
        for offset in (1024..1536).rev() {
            queue.push_no_check(make_payload(first.wrapping_add(offset), 1));
        }
        for offset in 512..1536 {
            assert_eq!(
                queue.pop(first.wrapping_add(offset)).unwrap().tsn,
                first.wrapping_add(offset)
            );
        }
        assert!(queue.is_empty());
        assert_eq!(queue.get_num_bytes(), 0);
    }
}

#[test]
fn reassembly_ordered_fragment_insertion_work_is_linear() {
    const COUNT: u32 = 1024;
    for first in [100u32, u32::MAX - COUNT / 2] {
        let fragment = |index: u32| ChunkPayloadData {
            tsn: first.wrapping_add(index),
            beginning_fragment: index == 0,
            ending_fragment: index == COUNT - 1,
            payload_type: PayloadProtocolIdentifier::Binary,
            user_data: Bytes::from(index.to_be_bytes().to_vec()),
            ..Default::default()
        };
        let mut chunks = Chunks::new(0, 0, PayloadProtocolIdentifier::Binary, vec![]);
        for index in 0..COUNT {
            assert_eq!(chunks.push(fragment(index)), index == COUNT - 1);
        }
        assert!(
            chunks.ordering_comparisons.get() <= COUNT as usize,
            "first TSN {first}: {} ordering comparisons for {COUNT} fragments",
            chunks.ordering_comparisons.get()
        );
        for index in [0, COUNT / 2, COUNT - 1] {
            assert!(!chunks.push(fragment(index)));
            assert_eq!(chunks.chunks.len(), COUNT as usize);
            assert!(chunks.is_complete());
        }
        let expected: Vec<u8> = (0..COUNT).flat_map(u32::to_be_bytes).collect();
        assert_eq!(
            chunks.to_payload(expected.len()).unwrap().as_ref(),
            expected
        );
    }
}

#[test]
fn reassembly_late_fragment_work_is_bounded() {
    const COUNT: u32 = 1024;
    for first_tsn in [12345u32, u32::MAX - COUNT / 2] {
        let mut chunks = Chunks::new(0, 0, PayloadProtocolIdentifier::Binary, vec![]);
        let fragment = |index: u32| ChunkPayloadData {
            tsn: first_tsn.wrapping_add(index),
            beginning_fragment: index == 0,
            ending_fragment: index == COUNT - 1,
            user_data: Bytes::from(index.to_be_bytes().to_vec()),
            ..Default::default()
        };
        assert!(!chunks.push(fragment(COUNT - 1)));
        for index in 0..COUNT - 1 {
            assert_eq!(chunks.push(fragment(index)), index == COUNT - 2);
        }
        let expected: Vec<u8> = (0..COUNT).flat_map(u32::to_be_bytes).collect();
        assert_eq!(
            chunks.to_payload(expected.len()).unwrap().as_ref(),
            expected
        );
        assert!(
            chunks.adjacency_checks.get() <= COUNT as usize * 3,
            "first TSN {first_tsn}: {} adjacency checks",
            chunks.adjacency_checks.get()
        );
    }
}

#[test]
fn reassembly_completeness_matches_fragment_adjacency() {
    fn contiguous(chunks: &Chunks) -> bool {
        chunks.chunks.first().is_some_and(|c| c.beginning_fragment)
            && chunks.chunks.last().is_some_and(|c| c.ending_fragment)
            && chunks
                .chunks
                .windows(2)
                .all(|pair| pair[1].tsn == pair[0].tsn.wrapping_add(1))
    }
    for first_tsn in [100u32, u32::MAX - 2] {
        let fragment = |index: u32| ChunkPayloadData {
            tsn: first_tsn.wrapping_add(index),
            beginning_fragment: index == 0,
            ending_fragment: index == 5,
            user_data: Bytes::from(vec![index as u8]),
            ..Default::default()
        };
        for order in [
            vec![],
            vec![0],
            vec![0, 1, 5],
            vec![0, 1, 2, 3, 4, 5],
            vec![0, 1, 1, 3, 4, 5],
            vec![0, 2, 1, 3, 4, 5],
        ] {
            let chunks = Chunks::new(
                0,
                0,
                PayloadProtocolIdentifier::Binary,
                order.into_iter().map(fragment).collect(),
            );
            assert_eq!(chunks.is_complete(), contiguous(&chunks));
        }
        for mut permutation in 0usize..720 {
            let mut available: Vec<u32> = (0..6).collect();
            let mut chunks = Chunks::new(0, 0, PayloadProtocolIdentifier::Binary, vec![]);
            for remaining in (1..=6).rev() {
                let index = available.remove(permutation % remaining);
                permutation /= remaining;
                assert_eq!(chunks.push(fragment(index)), contiguous(&chunks));
                let len = chunks.chunks.len();
                assert!(!chunks.push(fragment(index)), "duplicates stay rejected");
                assert_eq!(chunks.chunks.len(), len);
                assert_eq!(chunks.is_complete(), contiguous(&chunks));
            }
            assert!(chunks.is_complete());
            assert_eq!(chunks.to_payload(6).unwrap().as_ref(), &[0, 1, 2, 3, 4, 5]);
        }
    }
}

#[test]
fn reassembly_fragmented_ordered_message_uses_completion_order() {
    for first in [100u32, u32::MAX] {
        for ordered_completes_first in [false, true] {
            let mut queue = ReassemblyQueue::new(0);
            let fragment = |offset: u32, unordered, beginning, ending, value: &'static [u8]| {
                ChunkPayloadData {
                    tsn: first.wrapping_add(offset),
                    unordered,
                    beginning_fragment: beginning,
                    ending_fragment: ending,
                    payload_type: PayloadProtocolIdentifier::Binary,
                    user_data: Bytes::from_static(value),
                    ..Default::default()
                }
            };
            assert!(!queue.push(fragment(0, false, true, false, b"or")));
            let ordered_end = fragment(1, false, false, true, b"dered");
            let unordered = fragment(2, true, true, true, b"unordered");
            if ordered_completes_first {
                assert!(queue.push(ordered_end));
                assert!(queue.push(unordered));
            } else {
                assert!(queue.push(unordered));
                assert!(queue.push(ordered_end));
            }
            let expected: [&[u8]; 2] = if ordered_completes_first {
                [b"ordered", b"unordered"]
            } else {
                [b"unordered", b"ordered"]
            };
            for bytes in expected {
                assert_eq!(
                    queue.read().unwrap().to_payload(32).unwrap().as_ref(),
                    bytes
                );
            }
            assert!(queue.read().is_none());
            assert_eq!(queue.get_num_bytes(), 0);
        }
    }
}

#[test]
fn reassembly_complete_unordered_messages_skip_unrelated_fragment_scan() {
    for first in [100u32, u32::MAX - 512] {
        let mut queue = ReassemblyQueue::new(0);
        let fragment = |index: u32, complete| ChunkPayloadData {
            tsn: first.wrapping_add(if complete { index + 1024 } else { index * 2 }),
            unordered: true,
            beginning_fragment: true,
            ending_fragment: complete,
            payload_type: PayloadProtocolIdentifier::Binary,
            user_data: Bytes::from(index.to_be_bytes().to_vec()),
            ..Default::default()
        };
        for index in 0..1024 {
            assert!(!queue.push(fragment(index, false)));
        }
        queue.unordered_scan_steps = 0;
        for index in 1024..2048 {
            assert!(queue.push(fragment(index, true)));
            assert_eq!(
                queue.read().unwrap().to_payload(4).unwrap().as_ref(),
                index.to_be_bytes()
            );
        }
        assert_eq!(queue.unordered_chunks.len(), 1024);
        assert_eq!(queue.get_num_bytes(), 4096);
        assert_eq!(
            queue.unordered_scan_steps, 0,
            "complete messages must not scan retained fragments"
        );
        queue.forward_tsn_for_unordered(first.wrapping_add(2047));
        assert_eq!(queue.get_num_bytes(), 0);
        assert!(queue.unordered_chunks.is_empty());
    }
}

#[test]
fn payload_queue_gap_ack_limited_matches_full_report_prefix() {
    for cumulative in [100u32, u32::MAX - 4] {
        for offsets in [
            vec![],
            vec![1, 2, 3, 5, 6, 8, 65534, 65535, 65536, 65537],
            (1..=2048).collect(),
        ] {
            let mut queue = PayloadQueue::new();
            for &offset in &offsets {
                assert!(queue.push(make_payload(cumulative.wrapping_add(offset), 1), cumulative));
            }
            let mut expected: Vec<GapAckBlock> = Vec::new();
            for offset in offsets.iter().copied().filter(|&offset| offset <= 65535) {
                let offset = u16::try_from(offset).unwrap();
                if let Some(last) = expected.last_mut()
                    && last.end.checked_add(1) == Some(offset)
                {
                    last.end = offset;
                } else {
                    expected.push(GapAckBlock {
                        start: offset,
                        end: offset,
                    });
                }
            }
            for budget in [0, 1, 2, 3, 16, usize::MAX] {
                let actual = queue.get_gap_ack_blocks_limited(cumulative, budget);
                assert_eq!(actual.len(), expected.len().min(budget));
                for (actual, expected) in actual.iter().zip(&expected) {
                    assert_eq!((actual.start, actual.end), (expected.start, expected.end));
                }
                assert_eq!(queue.len(), offsets.len());
                assert_eq!(queue.get_num_bytes(), offsets.len());
            }
        }
    }
}

#[test]
fn reassembly_unordered_fragment_work_is_bounded() {
    const COUNT: u32 = 1024;
    for first in [100u32, u32::MAX - COUNT / 2] {
        for (order_index, order) in [
            (0..COUNT).collect::<Vec<_>>(),
            (0..COUNT).rev().collect(),
            (0..COUNT).step_by(2).chain((1..COUNT).step_by(2)).collect(),
        ]
        .into_iter()
        .enumerate()
        {
            let mut queue = ReassemblyQueue::new(0);
            for (position, &index) in order.iter().enumerate() {
                let complete = queue.push(ChunkPayloadData {
                    tsn: first.wrapping_add(index),
                    unordered: true,
                    beginning_fragment: index == 0,
                    ending_fragment: index == COUNT - 1,
                    payload_type: PayloadProtocolIdentifier::Binary,
                    user_data: Bytes::from(index.to_be_bytes().to_vec()),
                    ..Default::default()
                });
                assert_eq!(complete, position + 1 == COUNT as usize);
            }
            let expected: Vec<u8> = (0..COUNT).flat_map(u32::to_be_bytes).collect();
            assert_eq!(
                queue
                    .read()
                    .unwrap()
                    .to_payload(expected.len())
                    .unwrap()
                    .as_ref(),
                expected
            );
            assert_eq!(queue.get_num_bytes(), 0);
            assert!(queue.unordered_chunks.is_empty());
            if order_index == 0 {
                assert!(
                    queue.unordered_ordering_comparisons.get() <= COUNT as usize,
                    "first TSN {first}: {} ordering comparisons for {COUNT} in-order fragments",
                    queue.unordered_ordering_comparisons.get()
                );
            }
            assert!(
                queue.unordered_scan_steps <= COUNT as usize * 4,
                "{} fragment visits for {COUNT} fragments",
                queue.unordered_scan_steps
            );
        }
    }
}

#[test]
fn reassembly_unordered_runs_preserve_boundaries_in_every_arrival_order() {
    for first in [100u32, u32::MAX - 2] {
        for mut permutation in 0usize..720 {
            let mut available: Vec<u32> = (0..6).collect();
            let mut queue = ReassemblyQueue::new(0);
            let mut received = [false; 6];
            let mut delivered = [false; 2];
            for remaining in (1..=6).rev() {
                let index = available.remove(permutation % remaining);
                permutation /= remaining;
                let fragment = ChunkPayloadData {
                    tsn: first.wrapping_add(index),
                    unordered: true,
                    beginning_fragment: index.is_multiple_of(3),
                    ending_fragment: index % 3 == 2,
                    payload_type: PayloadProtocolIdentifier::Binary,
                    user_data: Bytes::from(vec![index as u8]),
                    ..Default::default()
                };
                received[index as usize] = true;
                let message = index as usize / 3;
                let complete = received[message * 3..message * 3 + 3].iter().all(|v| *v);
                assert_eq!(queue.push(fragment.clone()), complete);
                if complete {
                    let expected = [
                        message as u8 * 3,
                        message as u8 * 3 + 1,
                        message as u8 * 3 + 2,
                    ];
                    assert_eq!(
                        queue.read().unwrap().to_payload(3).unwrap().as_ref(),
                        expected
                    );
                    delivered[message] = true;
                } else {
                    let bytes = queue.get_num_bytes();
                    assert!(
                        !queue.push(fragment),
                        "retained duplicates do not complete a message"
                    );
                    assert_eq!(queue.get_num_bytes(), bytes);
                }
                assert!(queue.read().is_none());
                assert_eq!(
                    queue.get_num_bytes(),
                    received.iter().filter(|v| **v).count()
                        - delivered.iter().filter(|v| **v).count() * 3
                );
            }
            assert_eq!(delivered, [true, true]);
            assert!(queue.unordered_chunks.is_empty());
        }
    }
}

#[test]
fn reassembly_unordered_forward_tsn_cuts_runs_without_reviving_messages() {
    for first in [100u32, u32::MAX - 2] {
        for cut in 0..4u32 {
            let mut queue = ReassemblyQueue::new(0);
            let fragment = |index: u32| ChunkPayloadData {
                tsn: first.wrapping_add(index),
                unordered: true,
                beginning_fragment: index == 0 || index == 6,
                ending_fragment: index == 5 || index == 8,
                payload_type: PayloadProtocolIdentifier::Binary,
                user_data: Bytes::from(vec![index as u8]),
                ..Default::default()
            };
            for index in 0..4 {
                assert!(!queue.push(fragment(index)));
            }
            queue.forward_tsn_for_unordered(first.wrapping_add(cut));
            assert_eq!(queue.get_num_bytes(), 3 - cut as usize);
            for index in 4..6 {
                assert!(!queue.push(fragment(index)));
            }
            // The retained suffix has lost its beginning. The next message
            // must complete separately, including when its ending arrives first.
            assert!(!queue.push(fragment(8)));
            assert!(!queue.push(fragment(6)));
            assert!(queue.push(fragment(7)));
            assert_eq!(
                queue.read().unwrap().to_payload(3).unwrap().as_ref(),
                &[6, 7, 8]
            );
            assert!(queue.read().is_none());
            assert_eq!(queue.get_num_bytes(), 5 - cut as usize);
            queue.forward_tsn_for_unordered(first.wrapping_add(8));
            assert_eq!(queue.get_num_bytes(), 0);
            assert!(queue.unordered_chunks.is_empty());
            // Exercise the index again after every old fragment is abandoned.
            for index in [6, 7] {
                assert!(!queue.push(fragment(index)));
            }
            assert!(queue.push(fragment(8)));
            assert_eq!(
                queue.read().unwrap().to_payload(3).unwrap().as_ref(),
                &[6, 7, 8]
            );
        }
    }
}

// Issue #1178: application reads must not limit ordered protocol admission.
#[test]
fn reassembly_ordered_unread_messages_survive_full_sequence_wrap() {
    const COUNT: u32 = 65540;
    let mut queue = ReassemblyQueue::new(0);
    for sequence in 0..COUNT {
        assert!(
            queue.push(ChunkPayloadData {
                tsn: sequence.wrapping_add(u32::MAX - 200),
                stream_sequence_number: sequence as u16,
                beginning_fragment: true,
                ending_fragment: true,
                payload_type: PayloadProtocolIdentifier::Binary,
                user_data: Bytes::from(sequence.to_be_bytes().to_vec()),
                ..Default::default()
            }),
            "message {sequence} must be accepted before application reads"
        );
    }
    assert_eq!(queue.get_num_bytes(), COUNT as usize * 4);
    for sequence in 0..COUNT {
        assert_eq!(
            queue.read().unwrap().to_payload(4).unwrap().as_ref(),
            sequence.to_be_bytes()
        );
    }
    assert!(queue.read().is_none());
    assert_eq!(queue.get_num_bytes(), 0);
}

#[test]
fn reassembly_ordered_forward_preserves_ready_messages_and_completion_order() {
    for first in [10u16, u16::MAX - 1] {
        let mut queue = ReassemblyQueue::new(0);
        queue.next_ssn = first;
        let fragment = |sequence, unordered, ending, value: &'static [u8]| ChunkPayloadData {
            tsn: u32::from(sequence),
            stream_sequence_number: sequence,
            unordered,
            beginning_fragment: true,
            ending_fragment: ending,
            payload_type: PayloadProtocolIdentifier::Binary,
            user_data: Bytes::from_static(value),
            ..Default::default()
        };
        assert!(queue.push(fragment(first, false, true, b"ready")));
        assert!(!queue.push(fragment(first.wrapping_add(1), false, false, b"abandoned")));
        assert!(queue.push(fragment(first.wrapping_add(2), false, true, b"future")));
        assert!(queue.push(fragment(100, true, true, b"unordered")));
        queue.forward_tsn_for_ordered(first.wrapping_add(1));
        assert_eq!(queue.get_num_bytes(), b"readyfutureunordered".len());
        assert_eq!(queue.next_ssn, first.wrapping_add(3));
        queue.forward_tsn_for_ordered(first);
        assert_eq!(queue.next_ssn, first.wrapping_add(3));
        for expected in [b"ready".as_slice(), b"future", b"unordered"] {
            assert_eq!(
                queue.read().unwrap().to_payload(32).unwrap().as_ref(),
                expected
            );
        }
        assert!(queue.read().is_none());
        assert_eq!(queue.get_num_bytes(), 0);
    }
}

// Issue #1182: advancing abandonment must not rebuild every retained fragment.
#[test]
fn reassembly_unordered_forward_tsn_work_tracks_discarded_fragments() {
    const COUNT: u32 = 1024;
    for first in [100u32, u32::MAX - COUNT / 2] {
        let mut queue = ReassemblyQueue::new(0);
        let fragment = |index: u32, beginning, ending| ChunkPayloadData {
            tsn: first.wrapping_add(index),
            unordered: true,
            beginning_fragment: beginning,
            ending_fragment: ending,
            payload_type: PayloadProtocolIdentifier::Binary,
            user_data: Bytes::from(index.to_be_bytes().to_vec()),
            ..Default::default()
        };
        for index in 0..COUNT {
            assert!(!queue.push(fragment(index, index == 0, false)));
        }
        for index in 0..COUNT {
            queue.forward_tsn_for_unordered(first.wrapping_add(index));
            assert_eq!(queue.get_num_bytes(), (COUNT - index - 1) as usize * 4);
            assert!(!queue.is_readable());
        }
        assert!(queue.unordered_chunks.is_empty());
        assert!(
            queue.unordered_forward_steps <= COUNT as usize * 2,
            "{} fragment visits for {COUNT} one-fragment advances",
            queue.unordered_forward_steps
        );
        // Abandoned runs must not revive. A fresh reordered message still completes.
        assert!(!queue.push(fragment(COUNT + 2, false, true)));
        assert!(!queue.push(fragment(COUNT, true, false)));
        assert!(queue.push(fragment(COUNT + 1, false, false)));
        let expected: Vec<u8> = (COUNT..COUNT + 3).flat_map(u32::to_be_bytes).collect();
        assert_eq!(
            queue.read().unwrap().to_payload(12).unwrap().as_ref(),
            expected
        );
        assert_eq!(queue.get_num_bytes(), 0);
    }
}

#[test]
fn reassembly_unordered_forward_tsn_preserves_later_run_endpoints() {
    for first in [100u32, u32::MAX - 2] {
        let mut queue = ReassemblyQueue::new(0);
        let fragment = |index: u32| ChunkPayloadData {
            tsn: first.wrapping_add(index),
            unordered: true,
            beginning_fragment: index == 0 || index == 4,
            ending_fragment: index == 2 || index == 6,
            payload_type: PayloadProtocolIdentifier::Binary,
            user_data: Bytes::from(vec![index as u8]),
            ..Default::default()
        };
        for index in [0, 1, 4, 6] {
            assert!(!queue.push(fragment(index)));
        }
        queue.forward_tsn_for_unordered(first);
        assert_eq!(queue.get_num_bytes(), 3);
        assert!(queue.push(fragment(5)));
        assert_eq!(
            queue.read().unwrap().to_payload(3).unwrap().as_ref(),
            &[4, 5, 6]
        );
        assert!(!queue.push(fragment(2)), "the cut message has no beginning");
        assert_eq!(queue.get_num_bytes(), 2);
        assert!(queue.read().is_none());
        queue.forward_tsn_for_unordered(first.wrapping_add(2));
        assert_eq!(queue.get_num_bytes(), 0);
    }
}
