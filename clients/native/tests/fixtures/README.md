# Godot-offer service control

Apply `godot-service-offer.patch` to Godot client commit
`988797b4c2f2a870b946870b59046ea5a5f1a297` in a separate checkout.
Use `git apply --unidiff-zero /path/to/server/clients/native/tests/fixtures/godot-service-offer.patch`.
The zero-context format preserves GDScript tabs without patch-context whitespace.
The patch selects an answering reference host and a Godot offerer.
It keeps early service signals until the native peer is paired, counts signals
from the start of each phase, and records the gameplay generation before departure.
It also samples the native transport when reliable snapshot chunk 127 arrives.

Copy the patched `tests/smoke/webrtc_native_reference.rs` into the server's
`clients/native/examples/godot_service_probe.rs`, then build it:

```bash
cargo build --manifest-path clients/native/Cargo.toml --example godot_service_probe
```

Use the Godot client's Python environment and existing native/Web prerequisites.
Start a local Signal Fish Server that accepts the `godot-reference` app, then run:

```bash
python scripts/check-webrtc-reference.py \
  --godot /path/to/Godot_v4.7.2-stable_linux.arm64 \
  --native-extension /path/to/addons/webrtc_native \
  --native-reference-probe /path/to/server/clients/native/target/debug/examples/godot_service_probe \
  --reference-revision session-554-admission-counters \
  --runtime all --offerer godot --safe-receive --mixed-traffic \
  --credit-transport webrtc --service-url ws://127.0.0.1:3536/v3/ws \
  --require-gameplay --output /path/to/new-output-directory
```

Run the Python command from the patched Godot checkout. Remove the copied Rust
example after building. Keep the output directory, including failed attempts.
The control requires all 128 exact reliable snapshots per direction and phase,
32 outstanding snapshots, mixed delivery modes, selected-packet retirement,
and a fresh room and physical generation. Snapshot bodies retain the original
1 KiB size. HTTP carries coordination markers only; service WebSockets carry signaling.

This fixture qualifies two-seat local sessions. It refuses `--service-rejoin`:
reference-host departure changes host selection and needs a separate role-history
control. Remote networks, larger meshes, stable Godot lifetime behavior, and
the historical Godot 4.3 missing prefix remain open in server #1240/#1244 and
[Godot #355](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/355).
The general driver and host-departure follow-up is
[Godot #444](https://github.com/Ambiguous-Interactive/signal-fish-client-godot/issues/444).
