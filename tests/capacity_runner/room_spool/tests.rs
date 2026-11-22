use super::*;

#[test]
fn room_spool_preserves_exact_evidence_and_room_oracle_results() {
    use crate::config::{ChurnSchedule, DeliveryClass, Encoding};
    use crate::records::{ReceiptNames, RunRecords};
    use crate::schedule::build_run_shape;

    let mut config = crate::scenario_config(Encoding::V3Json);
    config.rooms = 2;
    config.churn = ChurnSchedule::None;
    let (plans, churn) = build_run_shape(&config).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("rooms");
    let spool = RoomSpool::create(&path, 2, 16_384, 1024 * 1024).unwrap();
    let mut expected = Vec::new();
    for room in 0..2 {
        let room_plans: Vec<_> = plans
            .iter()
            .filter(|plan| plan.room == room)
            .cloned()
            .collect();
        let mut records = crate::complete_records(&room_plans);
        if room == 1 {
            records.receipts.swap(0, 1);
            records.receipts.push(records.receipts[0].clone());
        }
        for sent in &records.sent {
            spool
                .push(room as usize, RoomEvent::Sent(sent.clone()))
                .unwrap();
        }
        for receipt in &records.receipts {
            spool
                .push(room as usize, RoomEvent::Receipt(receipt.clone()))
                .unwrap();
        }
        expected.push((room_plans, records));
    }
    spool.finish().unwrap();
    let replay = RoomReplay::open(&path, 2).unwrap();
    for (room, (plans, reference)) in expected.into_iter().enumerate() {
        let roster: Vec<_> = plans
            .iter()
            .map(|plan| (plan.name.clone(), plan.room))
            .collect();
        let row_budget = (reference.sent.len() + reference.receipts.len()) as u64;
        let mut actual = RunRecords::default();
        let mut names = ReceiptNames::default();
        assert!(replay
            .visit(room, row_budget - 1, |_, _| panic!("budget before decode"))
            .is_err());
        replay
            .visit(room, row_budget, |_, event| match event {
                RoomEvent::Sent(event) => actual.sent.push(names.share_sent(event)),
                RoomEvent::Receipt(event) => actual.receipts.push(names.share(event)),
                RoomEvent::Gap(event) => actual.gaps.push(names.share_gap(event)),
            })
            .unwrap();
        let summary = |records| {
            serde_json::to_value(crate::oracle::summarize(
                &plans,
                &roster,
                records,
                250_000,
                96,
                DeliveryClass::Reliable,
                &churn,
                None,
            ))
            .unwrap()
        };
        assert_eq!(
            serde_json::to_value(&actual).unwrap(),
            serde_json::to_value(&reference).unwrap(),
        );
        assert_eq!(summary(&actual), summary(&reference));
    }
    OpenOptions::new()
        .write(true)
        .open(room_path(&path, 1))
        .unwrap()
        .set_len(1)
        .unwrap();
    assert!(replay
        .visit(1, u64::MAX, |_, _| panic!("truncated room"))
        .is_err());
}

#[test]
fn room_spool_rejects_budget_and_write_failures_as_incomplete() {
    let temp = tempfile::tempdir().unwrap();
    let mut config = crate::scenario_config(crate::config::Encoding::V3Json);
    config.rooms = 1;
    let (plans, _) = crate::schedule::build_run_shape(&config).unwrap();
    let sent = RoomEvent::Sent(crate::complete_records(&plans).sent.remove(0));

    let limited = temp.path().join("limited");
    let spool = RoomSpool::create(&limited, 1, 1, 1).unwrap();
    spool.push(0, sent.clone()).unwrap();
    assert!(spool.finish().unwrap_err().contains("byte budget"));
    assert!(RoomReplay::open(&limited, 1).is_err());

    let blocked = temp.path().join("blocked");
    fs::create_dir(&blocked).unwrap();
    let blocked_room = room_path(&blocked, 0);
    File::create(&blocked_room).unwrap();
    let writer = BufWriter::new(File::open(&blocked_room).unwrap());
    let frames = [Frame {
        room: 0,
        event: sent.clone(),
    }];
    let error = write_rooms(&blocked, vec![Some(writer)], 1024, frames)
        .err()
        .unwrap();
    assert!(error.contains("flush room 0"), "{error}");
    assert!(RoomReplay::open(&blocked, 1).is_err());

    let rejected = temp.path().join("rejected");
    let spool = RoomSpool::create(&rejected, 1, 1, 1024).unwrap();
    let RoomEvent::Sent(mut sent) = sent else {
        unreachable!("fixture is a send");
    };
    sent.sender = "a".repeat(FRAME_LIMIT).as_str().into();
    assert!(spool
        .push(0, RoomEvent::Sent(sent))
        .unwrap_err()
        .contains("room event identity"));
    assert!(spool.finish().unwrap_err().contains("producer rejected"));
    assert!(RoomReplay::open(&rejected, 1).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn room_spool_opens_empty_room_files_before_capture() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("rooms");
    let spool = RoomSpool::create(&root, 3, 1, 1024).unwrap();
    for room in 0..3 {
        assert_eq!(fs::metadata(room_path(&root, room)).unwrap().len(), 0);
    }

    spool.finish().unwrap();
    let replay = RoomReplay::open(&root, 3).unwrap();
    replay.validate_ordinals(0).unwrap();
}

#[test]
fn room_spool_can_finish_sparse_capture_with_limited_descriptors() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("rooms");
    let rooms = 96;
    let spool = RoomSpool::create(&root, rooms, 1, 1024).unwrap();
    let config = crate::scenario_config(crate::config::Encoding::V3Json);
    let (plans, _) = crate::schedule::build_run_shape(&config).unwrap();
    let sent = RoomEvent::Sent(crate::complete_records(&plans).sent.remove(0));
    spool.push(rooms - 1, sent).unwrap();
    spool.finish().unwrap();

    let replay = RoomReplay::open(&root, rooms).unwrap();
    replay.validate_ordinals(1).unwrap();
}

#[test]
fn room_spool_preserves_the_first_rejection_after_a_later_send() {
    let temp = tempfile::tempdir().unwrap();
    let config = crate::scenario_config(crate::config::Encoding::V3Json);
    let (plans, _) = crate::schedule::build_run_shape(&config).unwrap();
    let mut records = crate::complete_records(&plans);
    let spool = RoomSpool::create(&temp.path().join("rooms"), 1, 8, 4096).unwrap();
    let log = crate::records::EventLog::for_partitioned_plans(&plans, spool).unwrap();
    let mut receipt = records.receipts.remove(0);
    receipt.sender = "a".repeat(FRAME_LIMIT).as_str().into();
    assert!(!log.push_receipt(receipt));
    log.push_sent(records.sent.remove(0));
    assert!(matches!(
        log.take_records().faults.as_slice(),
        [crate::oracle::InvalidReason::GeneratorRecordBudgetExceeded {
            first_excess_kind,
            allocation_failure: Some(detail),
            unretained_events: 2,
            ..
        }] if first_excess_kind == "receipt" && detail.contains("identity exceeds frame budget")
    ));
    assert!(log.finish_room_capture().is_err());
}

#[test]
fn room_spool_queue_refusal_never_commits_an_incomplete_index() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().to_path_buf();
    let (sender, receiver) = mpsc::sync_channel(1);
    let (resume, wait) = mpsc::channel();
    let path = root.clone();
    let queue = Arc::new(QueueDepth::new(1));
    let writer_queue = Arc::clone(&queue);
    let writer = thread::spawn(move || {
        wait.recv().unwrap();
        let frames = receiver.into_iter().inspect(|_| writer_queue.release());
        write_rooms(&path, vec![None], 1024, frames)
    });
    let spool = RoomSpool {
        root,
        sender,
        writer,
        rejected: AtomicBool::new(false),
        queue,
    };
    let config = crate::scenario_config(crate::config::Encoding::V3Json);
    let (plans, _) = crate::schedule::build_run_shape(&config).unwrap();
    let event = RoomEvent::Sent(crate::complete_records(&plans).sent.remove(0));
    spool.push(0, event.clone()).unwrap();
    let result = spool.push(0, event.clone());
    resume.send(()).unwrap();
    assert!(result.unwrap_err().contains("queue exhausted"));
    let queue = spool.capture_queue();
    assert_eq!(queue.capacity_frames, 1);
    assert_eq!(
        queue.pending_high_water_frames, 2,
        "one queued and one reserved frame"
    );

    assert!(spool.finish().unwrap_err().contains("producer rejected"));
    assert!(room_path(temp.path(), 0).is_file());
    assert!(RoomReplay::open(temp.path(), 1).is_err());
}

/// Compare replay memory on the same deterministic evidence in fresh processes.
#[test]
#[ignore = "run in a fresh process to measure peak RSS"]
fn room_spool_replay_memory_profile() {
    use crate::records::{DeliveryEventRef, RunRecords};
    use sha2::{Digest, Sha256};

    let variable = |name| std::env::var(name).expect(name);
    let rooms: usize = variable("CAPACITY_SPOOL_PROFILE_ROOMS").parse().unwrap();
    let sends = variable("CAPACITY_SPOOL_PROFILE_SENDS").parse().unwrap();
    assert!((1..=8).contains(&rooms) && (1..=5000).contains(&sends));
    let root = PathBuf::from(variable("CAPACITY_SPOOL_PROFILE_DIR"));
    let mode = variable("CAPACITY_SPOOL_PROFILE_MODE");
    let mut config = crate::scenario_config(crate::config::Encoding::V3Json);
    config.rooms = u32::try_from(rooms).unwrap();
    config.players_per_room = 16;
    config.warmup = std::time::Duration::ZERO;
    config.duration = std::time::Duration::from_micros(sends);
    config.send_rate_per_sender = 1_000_000.0;
    let (plans, _) = crate::schedule::build_run_shape(&config).unwrap();
    let rows = sends * 16 * 16;
    let mut digest = Sha256::new();
    let mut row_bytes = Vec::new();
    let mut hash_row = |row: DeliveryEventRef<'_>| {
        row_bytes.clear();
        serde_json::to_writer(&mut row_bytes, &row).unwrap();
        digest.update(&row_bytes);
        digest.update(b"\n");
    };
    match mode.as_str() {
        "write" => {
            let spool = root.join("rooms");
            fs::create_dir(&spool).unwrap();
            // Produce fixtures outside either measured replay. A blocking
            // source keeps synthetic bursts out of the live queue limit.
            let frames = (0..rooms).flat_map(|room| {
                let room_plans: Vec<_> = plans
                    .iter()
                    .filter(|plan| plan.room as usize == room)
                    .cloned()
                    .collect();
                let records = crate::complete_records(&room_plans);
                let frame = move |event: RoomEvent| Frame { room, event };
                records
                    .sent
                    .into_iter()
                    .map(RoomEvent::Sent)
                    .map(frame)
                    .chain(
                        records
                            .receipts
                            .into_iter()
                            .map(RoomEvent::Receipt)
                            .map(frame),
                    )
            });
            let index = write_rooms(
                &spool,
                open_writers(&spool, rooms).unwrap(),
                u64::MAX,
                frames,
            )
            .unwrap();
            crate::artifacts::write_json(spool.join(INDEX), &index).unwrap();
            crate::artifacts::write_deliveries(&root, &crate::complete_records(&plans)).unwrap();
            println!(
                "ROOM_SPOOL_BYTES {}",
                index.rooms.iter().map(|file| file.bytes).sum::<u64>()
            );
            return;
        }
        "legacy-eval" => {
            let records = crate::artifacts::read_records(&root).unwrap();
            let roster = plans
                .iter()
                .map(|plan| (plan.name.clone(), plan.room))
                .collect::<Vec<_>>();
            let summary = crate::oracle::summarize(
                &plans,
                &roster,
                &records,
                250_000,
                96,
                config.delivery_class,
                &crate::schedule::ChurnPlan::default(),
                None,
            );
            assert!(summary.valid, "{:?}", summary.reasons);
            digest.update(serde_json::to_vec(&summary).unwrap());
        }
        "partitioned-eval" => {
            let replay = RoomReplay::open(&root.join("rooms"), rooms).unwrap();
            let evaluation = crate::partition_oracle::evaluate_rooms(
                &replay,
                &config,
                &plans,
                &RunRecords::default(),
            )
            .unwrap()
            .expect("fixture admits room-wise evaluation");
            digest.update(serde_json::to_vec(&evaluation.summary).unwrap());
        }
        "legacy" => {
            let records = crate::artifacts::read_records(&root).unwrap();
            for room in 0..rooms {
                let members = |name: &str| {
                    plans
                        .iter()
                        .any(|plan| plan.room as usize == room && plan.name == name)
                };
                for row in records
                    .sent
                    .iter()
                    .filter(|event| event.room as usize == room)
                {
                    hash_row(DeliveryEventRef::Sent(row));
                }
                for row in records
                    .receipts
                    .iter()
                    .filter(|event| members(event.recipient.as_str()))
                {
                    hash_row(DeliveryEventRef::Receipt(row));
                }
            }
        }
        "partitioned" => {
            let replay = RoomReplay::open(&root.join("rooms"), rooms).unwrap();
            for room in 0..rooms {
                let mut records = RunRecords::default();
                let mut names = crate::records::ReceiptNames::default();
                replay
                    .visit(room, rows, |_, event| match event {
                        RoomEvent::Sent(event) => records.sent.push(names.share_sent(event)),
                        RoomEvent::Receipt(event) => records.receipts.push(names.share(event)),
                        RoomEvent::Gap(_) => panic!("unexpected gap in complete fixture"),
                    })
                    .unwrap();
                assert_eq!((records.sent.len() + records.receipts.len()) as u64, rows);
                for row in &records.sent {
                    hash_row(DeliveryEventRef::Sent(row));
                }
                for row in &records.receipts {
                    hash_row(DeliveryEventRef::Receipt(row));
                }
            }
        }
        _ => panic!("unknown room spool profile mode {mode}"),
    }
    println!(
        "ROOM_SPOOL_DIGEST {}",
        crate::diagnostics::finish_sha256(digest)
    );
}

#[test]
fn room_spool_replays_interleaved_rooms_in_capture_order() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("rooms");
    let spool = RoomSpool::create(&root, 2, 8, 4096).unwrap();
    let config = crate::scenario_config(crate::config::Encoding::V3Json);
    let (plans, _) = crate::schedule::build_run_shape(&config).unwrap();
    let mut event = crate::complete_records(&plans).sent.remove(0);
    for (ordinal, room) in [1, 0, 1, 0].into_iter().enumerate() {
        event.room = u32::try_from(room).unwrap();
        event.seq = ordinal as u64;
        spool.push(room, RoomEvent::Sent(event.clone())).unwrap();
    }
    spool.finish().unwrap();
    let replay = RoomReplay::open(&root, 2).unwrap();
    let mut observed = Vec::new();
    replay
        .visit_ordered(4, |event| {
            let RoomEvent::Sent(event) = event else {
                panic!("expected send")
            };
            observed.push((event.seq, event.room));
        })
        .unwrap();
    assert_eq!(observed, [(0, 1), (1, 0), (2, 1), (3, 0)]);
    assert!(replay
        .visit_ordered(3, |_| panic!("budget before visit"))
        .is_err());
}

#[test]
fn room_spool_captures_collector_events_without_retaining_raw_vectors() {
    use crate::records::{EventLog, ReceiptNames};
    use signal_fish_server::protocol::DeliveryGapReason;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("rooms");
    let spool = RoomSpool::create(&root, 2, 16_384, 1024 * 1024).unwrap();
    let mut config = crate::scenario_config(crate::config::Encoding::V3Json);
    config.rooms = 2;
    let (plans, churn) = crate::schedule::build_run_shape(&config).unwrap();
    let mut reference = crate::complete_records(&plans);
    let mut misroute = reference
        .receipts
        .iter()
        .find(|receipt| {
            reference.sent.iter().any(|send| {
                send.sender == receipt.sender
                    && send.seq == receipt.seq
                    && send.phase == crate::schedule::Phase::Measured
            })
        })
        .unwrap()
        .clone();
    misroute.received_us += 100_000;
    misroute.recipient = plans
        .iter()
        .find(|plan| plan.room == 1)
        .unwrap()
        .name
        .as_str()
        .into();
    reference.receipts.push(misroute);
    let mut unknown_sender = reference.receipts[0].clone();
    unknown_sender.sender = "unknown-source".into();
    reference.receipts.push(unknown_sender);
    let mut unknown_recipient = reference
        .receipts
        .iter()
        .find(|receipt| receipt.sender.starts_with("r1"))
        .unwrap()
        .clone();
    unknown_recipient.recipient = "unknown-recipient".into();
    reference.receipts.push(unknown_recipient);

    let receipt = reference.receipts.first().unwrap();
    reference.gaps.push(GapEvent {
        recipient: receipt.recipient.clone(),
        sender: receipt.sender.clone(),
        epoch: receipt.epoch,
        from_seq: 1,
        to_seq: 1,
        reason: DeliveryGapReason::VolatileDropped,
    });
    let log = EventLog::for_partitioned_plans(&plans, spool).unwrap();
    for sent in &reference.sent {
        log.push_sent(sent.clone());
    }
    for receipt in &reference.receipts {
        assert!(log.push_receipt(receipt.clone()));
    }
    for gap in &reference.gaps {
        log.push_gap(gap.clone());
    }
    log.finish_room_capture().unwrap();
    let mut control = log.take_records();
    assert!(control.sent.is_empty() && control.receipts.is_empty() && control.gaps.is_empty());

    let count = (reference.sent.len() + reference.receipts.len() + reference.gaps.len()) as u64;
    let replay = RoomReplay::open(&root, 2).unwrap();
    let bounded = crate::partition_oracle::evaluate_rooms(&replay, &config, &plans, &control)
        .unwrap()
        .expect("cross-room timing can be evaluated without restoring the run");
    let mut actual = Vec::new();
    let mut names = ReceiptNames::default();
    replay
        .visit_ordered(count, |event| {
            actual.push(serde_json::to_value(&event).unwrap());
            match event {
                RoomEvent::Sent(event) => control.sent.push(names.share_sent(event)),
                RoomEvent::Receipt(event) => control.receipts.push(names.share(event)),
                RoomEvent::Gap(event) => control.gaps.push(names.share_gap(event)),
            }
        })
        .unwrap();
    let expected: Vec<_> = reference
        .sent
        .iter()
        .cloned()
        .map(RoomEvent::Sent)
        .chain(reference.receipts.iter().cloned().map(RoomEvent::Receipt))
        .chain(reference.gaps.iter().cloned().map(RoomEvent::Gap))
        .map(|event| serde_json::to_value(event).unwrap())
        .collect();
    assert_eq!(actual, expected);
    let roster: Vec<_> = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect();
    let evaluate = |records| {
        crate::oracle::summarize(
            &plans,
            &roster,
            records,
            250_000,
            96,
            config.delivery_class,
            &churn,
            None,
        )
    };
    use crate::oracle::InvalidReason;
    let summary = evaluate(&control);
    assert!(summary
        .reasons
        .iter()
        .any(|reason| matches!(reason, InvalidReason::MisroutedDeliveries { count: 3, .. })));
    assert!(summary
        .reasons
        .iter()
        .any(|reason| matches!(reason, InvalidReason::InvalidGapReports { count: 1, .. })));
    assert_eq!(
        serde_json::to_value(summary).unwrap(),
        serde_json::to_value(evaluate(&reference)).unwrap(),
    );
    assert_eq!(
        serde_json::to_value(bounded.summary).unwrap(),
        serde_json::to_value(evaluate(&reference)).unwrap()
    );
}

#[test]
fn room_spool_preserves_exact_large_lag_quantile_under_declared_bound() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("rooms");
    let mut config = crate::scenario_config(crate::config::Encoding::V3Json);
    config.rooms = 2;
    config.generator_lag_bound = std::time::Duration::from_secs(10);
    let (plans, churn) = crate::schedule::build_run_shape(&config).unwrap();
    let mut records = crate::complete_records(&plans);
    for (ordinal, send) in records.sent.iter_mut().enumerate() {
        send.sent_us = send.intended_us + 1_000_001 + (ordinal as u64) * 256;
    }
    let log = crate::records::EventLog::for_partitioned_plans(
        &plans,
        RoomSpool::create(&root, 2, 2048, 1024 * 1024).unwrap(),
    )
    .unwrap();
    for send in &records.sent {
        log.push_sent(send.clone());
    }
    for receipt in &records.receipts {
        assert!(log.push_receipt(receipt.clone()));
    }
    log.finish_room_capture().unwrap();
    let replay = RoomReplay::open(&root, 2).unwrap();
    let roster: Vec<_> = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect();
    let expected = crate::oracle::summarize(
        &plans,
        &roster,
        &records,
        10_000_000,
        96,
        config.delivery_class,
        &churn,
        None,
    );
    assert!(expected.valid, "{:?}", expected.reasons);
    let actual =
        crate::partition_oracle::evaluate_rooms(&replay, &config, &plans, &log.take_records())
            .unwrap()
            .expect("declared lag bounds permit exact room-wise evaluation");
    assert_eq!(
        serde_json::to_value(actual.summary).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[test]
fn room_spool_evaluates_saturation_and_room_local_delivery_faults_exactly() {
    use crate::oracle::InvalidReason;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("rooms");
    let mut config = crate::scenario_config(crate::config::Encoding::V3Json);
    config.rooms = 2;
    let (plans, churn) = crate::schedule::build_run_shape(&config).unwrap();
    let mut records = crate::complete_records(&plans);
    let unsent = records.sent.pop().unwrap();
    records
        .receipts
        .retain(|receipt| receipt.sender != unsent.sender || receipt.seq != unsent.seq);
    records.receipts.swap(0, 1);
    records.receipts.push(records.receipts[0].clone());
    records.receipts.remove(3);
    let last = records.receipts.len() - 2;
    records.receipts.remove(last);
    let fault = InvalidReason::GeneratorSaturated {
        max_lag_us: 400_000,
        bound_us: 250_000,
    };
    records.faults.push(fault.clone());
    // Capture the cross-room gap before the room-zero gap to exercise
    // global arrival order and the full-roster identity check.
    for recipient in ["r1p0", "r0p0"] {
        records.gaps.push(GapEvent {
            recipient: recipient.into(),
            sender: "r0p1".into(),
            epoch: 1,
            from_seq: 1,
            to_seq: 1,
            reason: signal_fish_server::protocol::DeliveryGapReason::VolatileDropped,
        });
    }

    // Disconnect order in the run need not match shard evaluation order.
    for peer in ["r1p2", "unknown", "r0p2"] {
        records.disconnects.push(crate::records::DisconnectEvent {
            recipient: peer.into(),
            observation: crate::records::DisconnectObservation::StreamEnded,
        });
    }
    records.join_failures.push("r0p2: join detail".into());

    let log = crate::records::EventLog::for_partitioned_plans(
        &plans,
        RoomSpool::create(&root, 2, 2048, 1024 * 1024).unwrap(),
    )
    .unwrap();
    for send in &records.sent {
        log.push_sent(send.clone());
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
    for failure in &records.join_failures {
        log.push_join_failure(failure.clone());
    }
    log.push_fault(fault);
    log.finish_room_capture().unwrap();
    let replay = RoomReplay::open(&root, 2).unwrap();
    let roster: Vec<_> = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect();
    let expected = crate::oracle::summarize(
        &plans,
        &roster,
        &records,
        250_000,
        96,
        config.delivery_class,
        &churn,
        None,
    );
    assert_eq!(expected.totals.unsent, 1);
    assert_eq!(expected.totals.outstanding, 2);
    let actual =
        crate::partition_oracle::evaluate_rooms(&replay, &config, &plans, &log.take_records())
            .unwrap()
            .expect("room-local faults admit bounded exact evaluation");
    assert_eq!(
        serde_json::to_value(actual.summary).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[test]
fn room_spool_preserves_raw_fault_and_ledger_reason_precedence() {
    use crate::oracle::InvalidReason;

    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("rooms");
    let mut config = crate::scenario_config(crate::config::Encoding::V3Json);
    config.rooms = 11;
    let (plans, churn) = crate::schedule::build_run_shape(&config).unwrap();
    let mut records = crate::complete_records(&plans);
    for sender in ["r2p0", "r10p0"] {
        let sent = records
            .sent
            .iter_mut()
            .find(|event| event.sender == sender)
            .unwrap();
        sent.room = 0;
        sent.application_bytes = 95;
    }
    for recipient in ["r0p0", "r10p0"] {
        let receipt = records
            .receipts
            .iter_mut()
            .find(|event| event.recipient == recipient)
            .unwrap();
        receipt.received_us = 0;
        receipt.encoded_frame_body_bytes = 0;
    }
    let last_room = records
        .receipts
        .iter()
        .position(|event| event.recipient == "r10p0")
        .unwrap();
    records.receipts[..=last_room].rotate_right(1);
    let exempt = records
        .receipts
        .iter()
        .position(|event| event.recipient == "r2p0")
        .unwrap();
    records.receipts.remove(exempt);
    records.faults.push(InvalidReason::SlowConsumerDisconnect {
        recipients: vec!["r2p0".into()],
    });
    records.faults.push(InvalidReason::UnidentifiedGameData {
        recipient: "r2p0".into(),
        received_us: 1,
        application_bytes: 95,
        encoded_frame_body_bytes: 70,
        detail: "unknown source".into(),
    });

    let log = crate::records::EventLog::for_partitioned_plans(
        &plans,
        RoomSpool::create(&root, 11, 8192, 1024 * 1024).unwrap(),
    )
    .unwrap();
    for sent in &records.sent {
        log.push_sent(sent.clone());
    }
    for receipt in &records.receipts {
        assert!(log.push_receipt(receipt.clone()));
    }
    for reason in &records.faults {
        log.push_fault(reason.clone());
    }
    log.finish_room_capture().unwrap();
    let replay = RoomReplay::open(&root, 11).unwrap();
    let roster = plans
        .iter()
        .map(|plan| (plan.name.clone(), plan.room))
        .collect::<Vec<_>>();
    let expected = crate::oracle::summarize(
        &plans,
        &roster,
        &records,
        250_000,
        96,
        config.delivery_class,
        &churn,
        None,
    );
    assert!(
        matches!(&expected.reasons[2], InvalidReason::InvalidReceiptTimes { count: 2, first } if first.key.recipient == "r10p0")
    );
    assert!(
        matches!(&expected.reasons[3], InvalidReason::PayloadSizeMismatch { count: 3, first } if first.sender.as_deref() == Some("r10p0"))
    );
    assert!(
        matches!(&expected.reasons[4], InvalidReason::InvalidEncodedFrameBodySize { count: 3, first } if first.recipient.as_deref() == Some("r10p0"))
    );
    assert!(
        matches!(&expected.reasons[5], InvalidReason::InvalidScheduledSends { count: 2, first } if first.sender == "r10p0")
    );
    assert_eq!(expected.totals.outstanding, 1);
    assert!(!expected
        .reasons
        .iter()
        .any(|reason| matches!(reason, InvalidReason::OutstandingAtEnd { .. })));
    let actual =
        crate::partition_oracle::evaluate_rooms(&replay, &config, &plans, &log.take_records())
            .unwrap()
            .expect("raw and ledger violations admit bounded exact evaluation");
    assert_eq!(
        serde_json::to_value(actual.summary).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}
