//! Incoming work must not erase a T3 expiration that the caller has yet to service.

use super::*;
use crate::TimerConfig;
use crate::chunk::chunk_selective_ack::GapAckBlock;

const PAYLOAD: &[u8] = &[0x5a; 1024];

fn sent_messages(
    count: usize,
    reliability: ReliabilityType,
) -> Result<(Association, Instant, u32)> {
    let mut a = timed_test_association();
    a.cwnd = 16 * 1024;
    let now = Instant::now();
    let first = a.my_next_tsn;
    let ppi = PayloadProtocolIdentifier::Binary;
    let mut stream = a.open_stream(1, ppi)?;
    stream.set_reliability_params(true, reliability, 0)?;
    for _ in 0..count {
        stream.write_sctp(now, &Bytes::from_static(PAYLOAD), ppi)?;
    }
    assert_eq!(transmitted_data(&a.gather_outbound(now).0).len(), count);
    Ok((a, now, first))
}

#[test]
fn due_t3_is_not_postponed_by_duplicate_gap_ack() -> Result<()> {
    let (mut a, now, first) = sent_messages(2, ReliabilityType::Rexmit)?;
    let sack = ChunkSelectiveAck {
        cumulative_tsn_ack: first.wrapping_sub(1),
        advertised_receiver_window_credit: 65536,
        gap_ack_blocks: vec![GapAckBlock { start: 2, end: 2 }],
        ..Default::default()
    };
    a.handle_sack(&sack, now + Duration::from_millis(10))?;
    assert_eq!(a.inflight_queue.get(first).unwrap().miss_indicator, 1);
    assert!(!a.inflight_queue.get(first).unwrap().abandoned());
    let deadline = a.timers.get(Timer::T3RTX).unwrap();

    // A socket input can be serviced before an already-ready timeout. This
    // repeated gap ACK acknowledges neither new DATA nor the earliest TSN.
    let due = deadline + Duration::from_millis(1);
    a.handle_sack(&sack, due)?;
    assert_eq!(
        a.timers.get(Timer::T3RTX),
        Some(deadline),
        "a duplicate SACK must leave the pending expiration actionable"
    );
    assert_eq!(a.stats.get_num_t3timeouts(), 0);
    a.handle_timeout(due);
    assert_eq!(a.stats.get_num_t3timeouts(), 1);
    assert!(a.inflight_queue.get(first).unwrap().abandoned());
    assert_eq!(
        a.timers.get(Timer::T3RTX),
        Some(due + Duration::from_millis(a.rto_mgr.get_rto())),
        "timeout processing still starts the next recovery timer"
    );
    assert!(transmitted_data(&a.gather_outbound(due).0).is_empty());
    Ok(())
}

#[test]
fn due_t3_is_not_postponed_by_poll_transmit_of_new_data() -> Result<()> {
    let (mut a, _, first) = sent_messages(1, ReliabilityType::Rexmit)?;
    let deadline = a.timers.get(Timer::T3RTX).unwrap();
    let due = deadline + Duration::from_millis(1);
    let ppi = PayloadProtocolIdentifier::Binary;
    a.stream(1)?
        .write_sctp(due, &Bytes::from_static(PAYLOAD), ppi)?;
    let outbound = a.poll_transmit(due).expect("new DATA is ready");
    let Payload::RawEncode(raw) = outbound.message else {
        panic!("outbound SCTP is already encoded");
    };
    let sent = transmitted_data(&raw);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].tsn, first.wrapping_add(1));
    assert_eq!(sent[0].user_data.as_ref(), PAYLOAD);
    assert_eq!(a.inflight_queue.get(first).unwrap().nsent, 1);
    assert!(!a.inflight_queue.get(first).unwrap().abandoned());
    assert_eq!(
        a.timers.get(Timer::T3RTX),
        Some(deadline),
        "a new first transmission must not erase the older DATA's due timer"
    );

    a.handle_timeout(due);
    assert_eq!(a.stats.get_num_t3timeouts(), 1);
    assert!(a.inflight_queue.get(first).unwrap().abandoned());
    assert_eq!(
        a.timers.get(Timer::T3RTX),
        Some(due + Duration::from_millis(a.rto_mgr.get_rto()))
    );
    assert!(transmitted_data(&a.gather_outbound(due).0).is_empty());
    Ok(())
}

#[test]
fn earliest_ack_after_due_restarts_t3_for_remaining_data() -> Result<()> {
    let (mut a, _, first) = sent_messages(3, ReliabilityType::Reliable)?;
    let old_deadline = a.timers.get(Timer::T3RTX).unwrap();
    let ack_at = old_deadline + Duration::from_millis(1);
    a.handle_sack(
        &ChunkSelectiveAck {
            cumulative_tsn_ack: first,
            advertised_receiver_window_credit: 65536,
            ..Default::default()
        },
        ack_at,
    )?;
    // R3 deliberately restarts on genuine progress for the earliest TSN,
    // including when this ACK is serviced before an already-due timeout.
    let next_deadline = ack_at + Duration::from_millis(a.rto_mgr.get_rto());
    assert_eq!(a.timers.get(Timer::T3RTX), Some(next_deadline));
    assert_eq!(a.inflight_queue.len(), 2);
    a.handle_timeout(ack_at);
    assert_eq!(a.stats.get_num_t3timeouts(), 0);
    assert_eq!(
        a.inflight_queue.get(first.wrapping_add(1)).unwrap().nsent,
        1
    );

    a.handle_timeout(next_deadline);
    assert_eq!(a.stats.get_num_t3timeouts(), 1);
    let retried = transmitted_data(&a.gather_outbound(next_deadline).0);
    assert!(!retried.is_empty());
    assert_eq!(retried[0].tsn, first.wrapping_add(1));
    assert_eq!(retried[0].user_data.as_ref(), PAYLOAD);
    assert_eq!(
        a.inflight_queue.get(first.wrapping_add(1)).unwrap().nsent,
        2
    );
    Ok(())
}

#[test]
fn due_t3_survives_forward_only_sack_and_same_instant_is_idempotent() -> Result<()> {
    let (mut a, _, first) = sent_messages(1, ReliabilityType::Rexmit)?;
    a.rto_mgr.set_rto(1000, false);
    let first_timeout = a.timers.get(Timer::T3RTX).unwrap();
    a.handle_timeout(first_timeout);
    assert_eq!(a.stats.get_num_t3timeouts(), 1);
    assert_eq!(a.rto_mgr.get_rto(), 2000);
    assert_eq!(a.inflight_queue.get_num_bytes(), 0);
    assert_eq!(a.inflight_queue.len(), 1);
    assert!(a.inflight_queue.get(first).unwrap().abandoned());

    let has_forward = |packets: &[Bytes]| {
        packets.iter().any(|raw| {
            Packet::unmarshal(raw).unwrap().chunks.iter().any(|chunk| {
                chunk
                    .as_any()
                    .downcast_ref::<ChunkForwardTsn>()
                    .is_some_and(|forward| forward.new_cumulative_tsn == first)
            })
        })
    };
    let packets = a.gather_outbound(first_timeout).0;
    assert!(transmitted_data(&packets).is_empty());
    assert!(has_forward(&packets));
    let forward_deadline = a.timers.get(Timer::T3RTX).expect("RFC 3758 C5 timer");

    // No DATA remains to retry. A duplicate SACK that has not confirmed the
    // FORWARD TSN must still leave its due recovery timer intact.
    let due = forward_deadline + Duration::from_millis(1);
    a.handle_sack(
        &ChunkSelectiveAck {
            cumulative_tsn_ack: first.wrapping_sub(1),
            advertised_receiver_window_credit: 65536,
            ..Default::default()
        },
        due,
    )?;
    assert_eq!(a.timers.get(Timer::T3RTX), Some(forward_deadline));
    a.handle_timeout(due);
    assert_eq!(a.stats.get_num_t3timeouts(), 2);
    assert_eq!(a.rto_mgr.get_rto(), 4000);
    let rearmed = a.timers.get(Timer::T3RTX);
    assert_eq!(rearmed, Some(due + Duration::from_millis(4000)));
    let packets = a.gather_outbound(due).0;
    assert!(transmitted_data(&packets).is_empty());
    assert!(has_forward(&packets));

    // A caller may spuriously service the same instant again. The consumed
    // expiration must neither fire twice nor double RTO a second time.
    a.handle_timeout(due);
    assert_eq!(a.stats.get_num_t3timeouts(), 2);
    assert_eq!(a.rto_mgr.get_rto(), 4000);
    assert_eq!(a.timers.get(Timer::T3RTX), rearmed);

    a.handle_sack(
        &ChunkSelectiveAck {
            cumulative_tsn_ack: first,
            advertised_receiver_window_credit: 65536,
            ..Default::default()
        },
        due + Duration::from_millis(1),
    )?;
    assert!(a.inflight_queue.is_empty());
    assert!(a.timers.get(Timer::T3RTX).is_none());
    Ok(())
}

// Regression #1140: configured exhaustion must not leave timerless gameplay streams.
#[test]
fn recovery_retry_exhaustion_retires_all_streams() -> Result<()> {
    for timer in [Timer::T3RTX, Timer::T2Shutdown] {
        for retries in [0, 1, 5] {
            for shutdown_receiver in [false, true] {
                if timer == Timer::T3RTX && shutdown_receiver {
                    continue;
                }
                let mut a = timed_test_association();
                a.timers = TimerTable::new(TimerConfig {
                    max_t3_rtx_retrans: retries,
                    max_t2_shutdown_retrans: retries,
                    ..TimerConfig::default()
                });
                let now = Instant::now();
                let ppi = PayloadProtocolIdentifier::Binary;
                a.open_stream(1, ppi)?
                    .write_sctp(now, &Bytes::from_static(b"gameplay"), ppi)?;
                a.open_stream(2, ppi)?;
                assert_eq!(transmitted_data(&a.gather_outbound(now).0).len(), 1);
                if timer == Timer::T2Shutdown {
                    // Drain gameplay normally before testing loss of SHUTDOWN or its ACK.
                    a.handle_sack(
                        &ChunkSelectiveAck {
                            cumulative_tsn_ack: a.my_next_tsn.wrapping_sub(1),
                            advertised_receiver_window_credit: 65536,
                            ..Default::default()
                        },
                        now,
                    )?;
                    if shutdown_receiver {
                        a.handle_shutdown(
                            &ChunkShutdown {
                                cumulative_tsn_ack: a.my_next_tsn.wrapping_sub(1),
                            },
                            now,
                        )?;
                    } else {
                        a.shutdown()?;
                    }
                    assert!(!a.gather_outbound(now).0.is_empty());
                }
                for _ in 0..retries {
                    let at = a.timers.get(timer).expect("retry remains armed");
                    a.handle_timeout(at);
                    assert!(!a.gather_outbound(at).0.is_empty());
                    assert_ne!(a.state(), AssociationState::Closed);
                }
                let at = a.timers.get(timer).expect("final expiration remains armed");
                a.handle_timeout(at);
                assert_eq!(
                    a.state(),
                    AssociationState::Closed,
                    "timer={timer:?}, retries={retries}, receiver={shutdown_receiver}"
                );
                assert!(a.streams.is_empty());
                assert!(a.poll_timeout().is_none());
                assert!(a.gather_outbound(at).0.is_empty());
                assert!(a.open_stream(3, ppi).is_err());
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
                assert_eq!(
                    a.events
                        .iter()
                        .filter(|event| matches!(
                            event,
                            Event::AssociationLost {
                                reason: AssociationError::TimedOut,
                                ..
                            }
                        ))
                        .count(),
                    2
                );
                a.events.clear();
                a.handle_timeout(at + Duration::from_secs(1000));
                assert!(a.poll().is_none(), "terminal timeout notifies once");
            }
        }
    }
    Ok(())
}
