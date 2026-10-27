//! JSONL control probe for Godot native/Web channel qualification.
//! Use the pinned Godot driver documented in the fixture README.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use signal_fish_reference_native::engine::{
    ChannelProfile, Engine, EngineEvent, EngineSettings, MessageQueuePermit,
};
use signal_fish_server::protocol::{IceServer, PlayerId};
use std::io::{BufRead, Write};
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::{mpsc, oneshot};

type ProbeOutput = mpsc::UnboundedSender<(Value, Option<MessageQueuePermit>)>;

fn start_output(
    mut writer: impl Write + Send + 'static,
) -> anyhow::Result<(ProbeOutput, oneshot::Receiver<anyhow::Result<()>>)> {
    let (output, mut pending) = mpsc::unbounded_channel::<(Value, Option<MessageQueuePermit>)>();
    let (finished, stopped) = oneshot::channel();
    std::thread::Builder::new()
        .name("godot-probe-output".to_owned())
        .spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                while let Some((value, _permit)) = pending.blocking_recv() {
                    writeln!(writer, "{value}")?;
                    writer.flush()?;
                }
                Ok(())
            })();
            let _ = finished.send(result);
        })?;
    Ok((output, stopped))
}

fn enqueue(
    output: &ProbeOutput,
    mut value: Value,
    permit: Option<MessageQueuePermit>,
) -> anyhow::Result<()> {
    static START: OnceLock<Instant> = OnceLock::new();
    value["probe_us"] = json!(u64::try_from(
        START.get_or_init(Instant::now).elapsed().as_micros()
    )?);
    output
        .send((value, permit))
        .map_err(|_| anyhow::anyhow!("probe output writer stopped"))
}

fn emit(output: &ProbeOutput, value: Value) -> anyhow::Result<()> {
    enqueue(output, value, None)
}

async fn send_packet(
    output: &ProbeOutput,
    engine: &Engine,
    peer: PlayerId,
    value: &Value,
) -> anyhow::Result<()> {
    let channel = engine
        .channel(
            peer,
            value["label"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("missing label"))?,
        )
        .ok_or_else(|| anyhow::anyhow!("missing channel"))?;
    match value["op"].as_str().unwrap_or("") {
        "send" => {
            channel
                .send_text(
                    value["text"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("missing text"))?,
                )
                .await?;
        }
        "send_binary" => {
            let data = STANDARD.decode(
                value["data_base64"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("missing binary data"))?,
            )?;
            channel.send(data.as_slice().into()).await?;
        }
        _ => anyhow::bail!("invalid packet operation"),
    }
    emit(
        output,
        json!({"event":"sent", "generation":engine.diagnostic_snapshot(peer)["generation"], "label":value["label"],
        "marker":value["text"].as_str().map(|text| text.split(':').take(4).collect::<Vec<_>>().join(":"))}),
    )?;
    Ok(())
}

async fn transport_snapshot(engine: &Engine, peer: PlayerId) -> Value {
    let mut snapshot = engine.transport_diagnostic_snapshot(peer).await;
    if let Some(fields) = snapshot.as_object_mut() {
        let mut debt = serde_json::Map::new();
        for label in ["reliable", "ordered", "unreliable"] {
            if let Some(channel) = engine.channel(peer, label) {
                let value = match channel.outstanding_bytes().await {
                    Ok(bytes) => json!({"outstanding_bytes":bytes}),
                    Err(error) => json!({"error":error.to_string()}),
                };
                debt.insert(label.to_owned(), value);
            }
        }
        fields.insert("send_channels".to_owned(), Value::Object(debt));
    }
    snapshot
}

async fn receive_boundary(
    engine: &Engine,
    peer: PlayerId,
    generation: u64,
    label: &str,
    payload: &[u8],
    observed_generation: &mut Option<u64>,
) -> Option<Value> {
    if label != "reliable" {
        return None;
    }
    let text = std::str::from_utf8(payload).ok()?;
    let mut fields = text.splitn(5, ':');
    if fields.next()? != "snapshot" {
        return None;
    }
    let phase = fields.next()?;
    let sender = fields.next()?;
    let sequence: u32 = fields.next()?.parse().ok()?;
    let first = *observed_generation != Some(generation);
    *observed_generation = Some(generation);
    if !first && sequence != 127 {
        return None;
    }
    Some(json!({"generation":generation, "label":label,
        "marker":format!("snapshot:{phase}:{sender}:{sequence}"), "first":first, "last":sequence == 127,
        "snapshot":transport_snapshot(engine, peer).await}))
}

fn receive_rejection_event(event: &EngineEvent, snapshot: Value) -> Option<Value> {
    match event {
        EngineEvent::ChannelReceiveRejected {
            generation,
            label,
            failure,
            ..
        } => Some(json!({"event":"receive_rejected", "generation":generation,
                "label":label, "failure":failure, "snapshot":snapshot})),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (output, mut output_stopped) = start_output(std::io::stdout())?;
    let peer: PlayerId = "10000000-0000-0000-0000-000000000001".parse()?;
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (input_tx, mut input) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if input_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut engine = Engine::new(
        EngineSettings {
            channel_profile: ChannelProfile::Godot,
            ..EngineSettings::default()
        },
        events_tx,
    )?;
    let mut startup_phase = 0;
    let mut observed_snapshot_generation = None;
    let mut receive_failure: Option<Value> = None;
    loop {
        tokio::select! {
            stopped = &mut output_stopped => return stopped.map_err(|error| anyhow::anyhow!("probe writer task stopped: {error}"))?,
            line = input.recv() => {
                let Some(line) = line else {
                    engine.remove_peer(peer).await?;
                    break;
                };
                let value: Value = serde_json::from_str(&line)?;
                let id = value["id"].clone();
                let result: anyhow::Result<Value> = async {
                    if value["op"] == "snapshot" {
                        return Ok(match &receive_failure {
                            Some(failure) => failure["snapshot"].clone(),
                            None => transport_snapshot(&engine, peer).await,
                        });
                    }
                    if let Some(error) = &receive_failure {
                        anyhow::bail!("native receive rejected: {error}");
                    }
                    let snapshot = engine.diagnostic_snapshot(peer);
                    anyhow::ensure!(snapshot["receive_failure"].is_null(),
                        "native receive rejection awaits event drain: {}", snapshot["receive_failure"]);
                    match value["op"].as_str().unwrap_or("") {
                        "pair" => {
                            startup_phase = value["startup_phase"].as_u64().unwrap_or(0);
                            anyhow::ensure!(startup_phase <= 2, "invalid startup phase");
                            let servers: Vec<IceServer> = serde_json::from_value(value["ice_servers"].clone())?;
                            let offer = engine.pair_with(peer, value["offer"].as_bool().ok_or_else(|| anyhow::anyhow!("missing offer role"))?, &servers).await?;
                            Ok(json!({"offer":offer,"generation":engine.diagnostic_snapshot(peer)["generation"]}))
                        }
                        "offer" => Ok(json!(engine.handle_offer(peer, value["sdp"].as_str().ok_or_else(|| anyhow::anyhow!("missing SDP"))?.to_owned()).await?)),
                        "answer" => { engine.handle_answer(peer, value["sdp"].as_str().ok_or_else(|| anyhow::anyhow!("missing SDP"))?.to_owned()).await?; Ok(Value::Null) }
                        "candidate" => { engine.handle_remote_candidate(peer, value["candidate"].as_str().ok_or_else(|| anyhow::anyhow!("missing candidate"))?).await?; Ok(Value::Null) }
                        "send" | "send_binary" => { send_packet(&output, &engine, peer, &value).await?; Ok(Value::Null) }
                        "send_batch" => {
                            let packets = value["packets"].as_array().ok_or_else(|| anyhow::anyhow!("missing packets"))?;
                            anyhow::ensure!(!packets.is_empty() && packets.len() <= 256, "invalid packet batch size");
                            for packet in packets { send_packet(&output, &engine, peer, packet).await?; }
                            Ok(json!(packets.len()))
                        }
                        "close" => {
                            let channel = engine.channel(peer, value["label"].as_str().ok_or_else(|| anyhow::anyhow!("missing label"))?).ok_or_else(|| anyhow::anyhow!("missing channel"))?;
                            channel.close().await?;
                            Ok(Value::Null)
                        }
                        "remove" => { engine.remove_peer(peer).await?; Ok(json!(!engine.is_paired(peer))) }
                        _ => anyhow::bail!("unknown operation"),
                    }
                }.await;
                match result {
                    Ok(result) => emit(&output,json!({"id": id, "result": result}))?,
                    Err(error) => emit(&output,json!({"id": id, "error": error.to_string()}))?,
                }
            }
            Some(event) = events.recv() => {
                if !engine.is_current_event(&event) { continue; }
                if let Some(mut failure) = receive_rejection_event(&event, Value::Null) {
                    failure["snapshot"] = transport_snapshot(&engine, peer).await;
                    emit(&output,failure.clone())?;
                    receive_failure = Some(failure);
                    engine.remove_peer(peer).await?;
                    continue;
                }
                let (_, generation) = event.peer_generation();
                match event {
                    EngineEvent::LocalCandidate {candidate_json,..} => emit(&output,json!({"event":"candidate", "generation":generation, "candidate":candidate_json}))?,
                    EngineEvent::ChannelOpen {label,..} => {
                        engine.note_channel_open(peer,&label).await;
                        let channel = engine.channel(peer, &label).ok_or_else(|| anyhow::anyhow!("open channel disappeared"))?;
                        if startup_phase > 0 && label == "reliable" {
                            for sequence in 0..32 {
                                let data = format!("startup:{startup_phase}:reference:{sequence}:{}", "0123456789abcdef".repeat(64));
                                channel.send(data.as_bytes().into()).await?;
                            }
                            emit(&output,json!({"event":"startup_sent", "generation":generation, "count":32}))?;
                        }
                        emit(&output,json!({"event":"open", "generation":generation, "label":label,
                            "properties": {"internal_id":channel.id(), "negotiated":channel.negotiated().await?,
                                "ordered":channel.ordered().await?,
                                "lifetime":channel.max_packet_life_time().await?,
                                "retransmits":channel.max_retransmits().await?}}))?;
                    }
                    EngineEvent::PcState {state,..} => emit(&output,json!({"event":"state", "generation":generation, "state":state.to_string()}))?,
                    EngineEvent::ChannelMessage {label,text,queue_permit,..} => {
                        let boundary = receive_boundary(&engine, peer, generation, &label, text.as_bytes(), &mut observed_snapshot_generation).await;
                        let mut row = json!({"event":"message", "generation":generation, "label":label,"text":text});
                        if let Some(boundary) = boundary { row["receive_boundary"] = boundary; }
                        enqueue(&output, row, Some(queue_permit))?;
                    }
                    EngineEvent::ChannelBinaryMessage {label,data,queue_permit,..} => {
                        let boundary = receive_boundary(&engine, peer, generation, &label, &data, &mut observed_snapshot_generation).await;
                        let mut row = json!({"event":"binary", "generation":generation, "label":label,"data_base64":STANDARD.encode(data)});
                        if let Some(boundary) = boundary { row["receive_boundary"] = boundary; }
                        enqueue(&output, row, Some(queue_permit))?;
                    }
                    EngineEvent::ChannelClosed {label,..} => emit(&output,json!({"event":"closed", "generation":generation, "label":label, "snapshot":transport_snapshot(&engine, peer).await}))?,
                    _ => {}
                }
            }
            else => break,
        }
    }
    drop(output);
    output_stopped
        .await
        .map_err(|error| anyhow::anyhow!("probe writer task stopped: {error}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use signal_fish_reference_native::engine::{ChannelReceiveFailure, ReceiveRejection};

    #[test]
    fn terminal_record_keeps_exact_failure_when_link_snapshot_is_missing_or_changes() {
        let peer = PlayerId::from_u128(0x1264);
        for (reason, messages, bytes) in [
            (ReceiveRejection::MessageBytes, 0, 0),
            (ReceiveRejection::PendingMessages, 256, 1048576),
            (ReceiveRejection::PendingBytes, 128, 1048576),
        ] {
            let failure = ChannelReceiveFailure {
                reason,
                message_bytes: 8192,
                pending_messages: messages,
                pending_bytes: bytes,
            };
            let event = EngineEvent::ChannelReceiveRejected {
                peer,
                generation: 7,
                label: "reliable".to_owned(),
                failure,
            };
            for mut snapshot in [
                Value::Null,
                json!({"generation":7, "receive_failure":failure}),
            ] {
                let record = receive_rejection_event(&event, snapshot.clone()).unwrap();
                snapshot = json!({"generation":8, "receive_failure":null});
                assert_eq!(record["failure"], serde_json::to_value(failure).unwrap());
                assert_eq!(record["generation"], 7);
                assert_eq!(record["label"], "reliable");
                assert_ne!(record["snapshot"], snapshot);
            }
        }
    }

    #[tokio::test]
    async fn receive_boundaries_keep_the_actual_first_sequence_and_fresh_generation() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let engine = Engine::new(
            EngineSettings {
                crippled: true,
                channel_profile: ChannelProfile::Godot,
                ..EngineSettings::default()
            },
            events,
        )
        .unwrap();
        let peer = PlayerId::from_u128(0x1244);
        let mut observed = None;
        for payload in [
            b"snapshot:2:godot:invalid:body".as_slice(),
            b"snapshot:2:godot:\xff",
        ] {
            assert!(
                receive_boundary(&engine, peer, 7, "reliable", payload, &mut observed)
                    .await
                    .is_none()
            );
            assert_eq!(observed, None);
        }
        assert!(receive_boundary(
            &engine,
            peer,
            7,
            "unreliable",
            b"snapshot:2:godot:0:body",
            &mut observed
        )
        .await
        .is_none());
        assert_eq!(observed, None);
        for (generation, sequence, expected) in [
            (7, 8, Some((true, false))),
            (7, 9, None),
            (7, 127, Some((false, true))),
            (8, 0, Some((true, false))),
        ] {
            let payload = format!("snapshot:2:godot:{sequence}:body");
            let boundary = receive_boundary(
                &engine,
                peer,
                generation,
                "reliable",
                payload.as_bytes(),
                &mut observed,
            )
            .await;
            match expected {
                Some((first, last)) => {
                    let boundary = boundary.unwrap();
                    assert_eq!(boundary["generation"], generation);
                    assert_eq!(boundary["marker"], format!("snapshot:2:godot:{sequence}"));
                    assert_eq!(boundary["first"], first);
                    assert_eq!(boundary["last"], last);
                }
                None => assert!(boundary.is_none()),
            }
        }
    }

    #[tokio::test]
    async fn output_failure_reaches_the_idle_owner() {
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("closed output"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (output, stopped) = start_output(FailedWriter).unwrap();
        emit(&output, json!({"event":"open"})).unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), stopped)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_err(),
            "writer failure must terminate an idle probe"
        );
        assert!(emit(&output, json!({"event":"closed"})).is_err());
    }

    #[test]
    fn physical_close_cannot_be_reported_as_receive_rejection() {
        let closed = EngineEvent::ChannelClosed {
            peer: PlayerId::from_u128(0x1264),
            generation: 7,
            label: "reliable".to_owned(),
        };
        assert!(receive_rejection_event(&closed, json!({"receive_failure":null})).is_none());
    }
}
