//! Export exact relay frames for the isolated compression study.

#[path = "support/relay_serialization.rs"]
mod relay_serialization;

use axum::extract::ws::Message;
use base64::Engine;
use relay_serialization::{assert_expected_output_digest, Fixture, Scenario, ROOM_SIZES};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::{BufWriter, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut directory = None;
    let mut validate_only = false;
    for argument in std::env::args_os().skip(1) {
        if argument == "--test" {
            validate_only = true;
        } else if argument == "--help" {
            println!("Usage: relay_compression_frames [output-directory | --test]");
            return Ok(());
        } else if argument == "--list" {
            return Ok(());
        } else if argument != "--bench" {
            if argument.to_string_lossy().starts_with('-') {
                return Err("unknown option; pass an output directory or --test".into());
            }
            if directory
                .replace(std::path::PathBuf::from(argument))
                .is_some()
            {
                return Err("pass exactly one output directory".into());
            }
        }
    }
    if directory.is_some() && validate_only {
        return Err("choose an output directory or --test".into());
    }
    if let Some(directory) = &directory {
        std::fs::create_dir_all(directory)?;
    }
    let mut cases = Vec::new();
    for scenario in Scenario::ALL {
        for room_size in ROOM_SIZES {
            let mut fixture = Fixture::new(room_size, scenario);
            let (ledger, frames) = fixture.captured_sample();
            assert_expected_output_digest(scenario, room_size, &ledger);
            let Some(directory) = &directory else {
                continue;
            };
            let filename = format!("{}-{room_size}.jsonl", scenario.name());
            let mut output = BufWriter::new(std::fs::File::create(directory.join(&filename))?);
            let mut digest = Sha256::new();
            for (recipient, frame) in &frames {
                let (kind, bytes) = match frame {
                    Message::Text(text) => ("text", text.as_bytes()),
                    Message::Binary(bytes) => ("binary", bytes.as_ref()),
                    _ => return Err("captured relay is not a data frame".into()),
                };
                let row = json!({
                    "recipient": recipient,
                    "kind": kind,
                    "payload": base64::prelude::BASE64_STANDARD.encode(bytes),
                });
                let mut encoded = serde_json::to_vec(&row)?;
                encoded.push(b'\n');
                digest.update(&encoded);
                output.write_all(&encoded)?;
            }
            output.flush()?;
            cases.push(json!({
                "file": filename,
                "scenario": scenario.name(),
                "room_size": room_size,
                "frames": frames.len(),
                "wire_bytes": ledger.wire_bytes,
                "unsupported_format": ledger.unsupported_format,
                "output_sha256": ledger.output_sha256.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                "corpus_sha256": digest.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            }));
        }
    }
    if let Some(directory) = &directory {
        std::fs::write(
            directory.join("manifest.json"),
            serde_json::to_vec_pretty(&json!({
                "version": 1,
                "executable_sha256": Sha256::digest(std::fs::read(std::env::current_exe()?)?).iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                "cases": cases,
            }))?,
        )?;
    }
    Ok(())
}
