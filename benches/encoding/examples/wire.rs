#[allow(warnings, clippy::all)]
#[path = "../generated/game_capnp.rs"]
pub mod game_capnp;
#[path = "../support/mod.rs"]
mod support;

fn main() {
    for fixture in support::fixtures() {
        for codec in support::Codec::ALL {
            if !matches!(codec, support::Codec::Prost | support::Codec::Varint) {
                continue;
            }
            let bytes = support::validate(codec, &fixture);
            println!(
                "{}",
                serde_json::json!({
                    "fixture": fixture.name,
                    "codec": codec.name(),
                    "wire": bytes,
                    "sha256": fixture.sha256,
                    "source_bytes": fixture.source_bytes,
                })
            );
        }
    }
}
