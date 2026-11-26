use super::*;
use crate::oracle::InvalidReason;
use crate::records::{
    sort_sent_events, DisconnectEvent, DisconnectObservation, EventLog, GapEvent,
};
use crate::room_spool::RoomSpool;

#[test]
fn bounded_room_replay_keeps_the_run_wide_duplicate_allowance_in_one_room() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = crate::scenario_config(Encoding::V3Json);
    config.rooms = 2;
    config.planned_record_budget = config.validate_planned_record_budget().unwrap();
    let (plans, churn) = crate::schedule::build_run_shape(&config).unwrap();
    let roster = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect::<Vec<_>>();
    let mut expected = crate::complete_records(&plans);
    let duplicate = expected.receipts[0].clone();
    // Concentrate the receive allowance and the retained crossing row in
    // the first room, after collecting the full offered schedule.
    let peers = u64::from(config.rooms) * u64::from(config.players_per_room);
    let duplicates = 1 + crate::records::extra_inbound_events(peers).unwrap();
    for _ in 0..duplicates {
        expected.receipts.push(duplicate.clone());
    }
    let root = temp.path().join("rooms");
    let log = EventLog::for_partitioned_plans(
        &plans,
        RoomSpool::create(&root, config.rooms as usize, 16_384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    for sent in &expected.sent {
        log.push_sent(sent.clone());
    }
    for (index, receipt) in expected.receipts.iter().enumerate() {
        assert_eq!(
            log.push_receipt(receipt.clone()),
            index + 1 < expected.receipts.len(),
            "the final receipt crosses the budget and is retained"
        );
    }
    log.finish_room_capture().unwrap();
    let replay = RoomReplay::open(&root, config.rooms as usize).unwrap();
    let control = log.take_records();
    assert!(!control.faults.is_empty());
    expected.faults.clone_from(&control.faults);
    let actual = evaluate_rooms(&replay, &config, &plans, &control)
        .unwrap()
        .expect("duplicates within the accepted allowance admit bounded replay");
    assert!(replay
        .restore_hot_records(
            &mut RunRecords::default(),
            config.planned_record_budget,
            u64::MAX
        )
        .is_err());
    let (capture_rows, _) = crate::room_spool::row_limits(&config).unwrap();
    let mut tight = config.clone();
    tight.raw_event_budget_bytes = config.validate_partitioned_room_budget().unwrap();
    assert!(evaluate_rooms(&replay, &tight, &plans, &control)
        .unwrap()
        .is_none());
    let mut refused = RunRecords::default();
    assert!(replay
        .restore_hot_records(&mut refused, capture_rows, tight.raw_event_budget_bytes)
        .unwrap_err()
        .contains("exceed replay byte budget"));
    let mut restored = control;
    replay
        .restore_hot_records(&mut restored, capture_rows, config.raw_event_budget_bytes)
        .unwrap();
    assert_eq!(
        serde_json::to_value(restored.receipts).unwrap(),
        serde_json::to_value(&expected.receipts).unwrap(),
    );
    let expected = oracle::summarize(
        &plans,
        &roster,
        &expected,
        micros(config.generator_lag_bound),
        config.payload_bytes,
        config.delivery_class,
        &churn,
        None,
    );
    assert!(!expected.valid);
    assert_eq!(
        serde_json::to_value(actual.summary).unwrap(),
        serde_json::to_value(expected).unwrap(),
    );
}

#[test]
fn bounded_room_replay_matches_mixed_faults_across_capture_orders() {
    let temp = tempfile::tempdir().unwrap();
    for seed in 0..24 {
        let mut rng = fastrand::Rng::with_seed(seed);
        let mut config = crate::scenario_config(Encoding::V3Json);
        config.rooms = rng.u32(2..=4);
        let (plans, churn) = crate::schedule::build_run_shape(&config).unwrap();
        let roster: Vec<_> = plans
            .iter()
            .map(|plan| (plan.name.clone(), plan.room))
            .collect();
        let mut records = crate::complete_records(&plans);

        let sent_index = rng.usize(..records.sent.len());
        let mut duplicate = records.sent[sent_index].clone();
        duplicate.intended_us += 7;
        if seed % 2 == 0 {
            duplicate.epoch = 2;
        }
        records.sent.push(duplicate);

        let mut unknown_send = records.sent[rng.usize(..records.sent.len())].clone();
        unknown_send.sender = "foreign-source".into();
        unknown_send.application_bytes = 95;
        records.sent.push(unknown_send.clone());

        let receipt_index = rng.usize(..records.receipts.len());
        records.receipts.remove(receipt_index);
        let mut receipt = records.receipts[rng.usize(..records.receipts.len())].clone();
        receipt.sender = unknown_send.sender;
        receipt.seq = unknown_send.seq;
        receipt.epoch = unknown_send.epoch;
        receipt.received_us = 0;
        receipt.application_bytes = 95;
        receipt.encoded_frame_body_bytes = 70;
        records.receipts.push(receipt.clone());
        receipt.recipient = "foreign-recipient".into();
        records.receipts.push(receipt);
        for _ in 0..8 {
            let left = rng.usize(..records.receipts.len());
            let right = rng.usize(..records.receipts.len());
            records.receipts.swap(left, right);
        }
        let first = &records.receipts[0];
        records.gaps.push(GapEvent {
            recipient: first.recipient.clone(),
            sender: first.sender.clone(),
            epoch: first.epoch,
            from_seq: first.server_seq,
            to_seq: first.server_seq,
            reason: signal_fish_server::protocol::DeliveryGapReason::VolatileDropped,
        });
        let peer = plans[rng.usize(..plans.len())].name.clone();
        records.disconnects.push(DisconnectEvent {
            recipient: peer.clone(),
            observation: DisconnectObservation::StreamEnded,
        });
        records.faults.push(InvalidReason::SlowConsumerDisconnect {
            recipients: vec![peer],
        });

        let root = temp.path().join(seed.to_string());
        let log = EventLog::for_partitioned_plans(
            &plans,
            RoomSpool::create(&root, config.rooms as usize, 2048, 1024 * 1024).unwrap(),
        )
        .unwrap();
        for sent in &records.sent {
            log.push_sent(sent.clone());
        }
        for receipt in &records.receipts {
            assert!(log.push_receipt(receipt.clone()));
        }
        for gap in &records.gaps {
            log.push_gap(gap.clone());
        }
        for disconnect in &records.disconnects {
            log.push_disconnect(disconnect.clone());
        }
        for fault in &records.faults {
            log.push_fault(fault.clone());
        }
        log.finish_room_capture().unwrap();
        let replay = RoomReplay::open(&root, config.rooms as usize).unwrap();
        let actual = evaluate_rooms(&replay, &config, &plans, &log.take_records())
            .unwrap()
            .expect("supported mixed faults admit bounded replay");

        sort_sent_events(&mut records.sent);
        let expected = oracle::summarize(
            &plans,
            &roster,
            &records,
            micros(config.generator_lag_bound),
            config.payload_bytes,
            config.delivery_class,
            &churn,
            None,
        );
        assert!(!expected.valid);
        let expected_histogram =
            oracle::latency_histogram(oracle::latency_pairs(&records).map(|(_, latency)| latency));
        let buckets = |histogram: &Histogram<u64>| {
            histogram
                .iter_recorded()
                .map(|bucket| (bucket.value_iterated_to(), bucket.count_at_value()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            buckets(&actual.histogram),
            buckets(&expected_histogram),
            "seed {seed}",
        );
        assert_eq!(
            serde_json::to_value(actual.summary).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "seed {seed}",
        );
    }
}
