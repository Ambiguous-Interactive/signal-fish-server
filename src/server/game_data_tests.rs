use super::game_data::one_shot_arc_builder;
use crate::protocol::ServerMessage;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct DropProbe(Arc<AtomicUsize>);

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn one_shot_arc_builder_consumes_builder_once_and_defends_against_repeat_calls() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_for_builder = Arc::clone(&calls);
    let mut builder = one_shot_arc_builder(move || {
        calls_for_builder.fetch_add(1, Ordering::Relaxed);
        Some(ServerMessage::Pong)
    });

    assert!(matches!(builder().as_deref(), Some(ServerMessage::Pong)));
    assert!(
        builder().is_none(),
        "a repeated call must cancel defensively"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn one_shot_arc_builder_preserves_cancellation_and_drops_called_capture() {
    let drops = Arc::new(AtomicUsize::new(0));
    let probe = DropProbe(Arc::clone(&drops));
    let mut builder = one_shot_arc_builder(move || {
        drop(probe);
        None
    });

    assert!(builder().is_none(), "a missing stamp must cancel the relay");
    assert!(builder().is_none(), "cancellation remains one-shot");
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}

#[test]
fn one_shot_arc_builder_drops_uncalled_builder_capture() {
    let drops = Arc::new(AtomicUsize::new(0));
    let probe = DropProbe(Arc::clone(&drops));
    let builder = one_shot_arc_builder(move || {
        let _probe = probe;
        Some(ServerMessage::Pong)
    });

    drop(builder);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
