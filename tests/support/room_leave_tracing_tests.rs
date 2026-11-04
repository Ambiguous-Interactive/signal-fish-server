use super::*;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

type LeaveFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

async fn paused_departure() -> (
    Arc<EnhancedGameServer>,
    PlayerId,
    mpsc::Receiver<Arc<ServerMessage>>,
    RoomEventMutationGuard,
) {
    let server = create_test_server().await;
    let player = PlayerId::new_v4();
    let (sender, receiver) = mpsc::channel(16);
    server.connect_client(player, sender).await;
    server
        .handle_join_room(
            &player,
            "trace-room".into(),
            Some("TRACE1".into()),
            "Actor".into(),
            Some(2),
            Some(true),
            None,
            None,
            None,
        )
        .await;
    let room = server.get_client_room(&player).await.expect("joined room");
    let gate = server
        .message_coordinator
        .lock_room_event_mutation(&room)
        .await;
    (server, player, receiver, gate)
}

#[derive(Clone)]
struct DepartureSpanEntries(Arc<AtomicBool>);

impl<S> tracing_subscriber::Layer<S> for DepartureSpanEntries
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_enter(&self, id: &tracing::span::Id, context: tracing_subscriber::layer::Context<'_, S>) {
        if context
            .span(id)
            .is_some_and(|span| span.name() == "room.leave")
        {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}

fn subscriber() -> tracing::Dispatch {
    tracing::Dispatch::new(
        tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::io::sink),
        ),
    )
}

#[tokio::test]
async fn pending_room_leave_does_not_enter_unrelated_task_context() {
    let (server, player, _receiver, gate) = paused_departure().await;
    let entries = Arc::new(AtomicBool::new(false));
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(DepartureSpanEntries(entries.clone()))
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(std::io::sink),
            ),
    );
    let mut leave = Box::pin(
        server
            .leave_room_locked(&player, false)
            .with_subscriber(dispatch.clone()),
    );
    assert!(futures_util::poll!(tokio::task::unconstrained(leave.as_mut())).is_pending());
    assert!(
        entries.load(Ordering::Relaxed),
        "the pending departure must enter room.leave while being polled"
    );
    let current = tracing::dispatcher::with_default(&dispatch, || {
        tracing::Span::current()
            .metadata()
            .map(|metadata| metadata.name())
    });
    assert_eq!(
        current, None,
        "a pending room departure leaked its trace span"
    );
    drop(leave);
    drop(gate);
}

#[tokio::test]
async fn room_leave_canceled_on_another_worker_does_not_break_logging() {
    let (server, player, _receiver, gate) = paused_departure().await;
    let dispatch = subscriber();
    let runtime = tokio::runtime::Handle::current();
    let other_runtime = runtime.clone();
    let (send_future, receive_future) = std::sync::mpsc::sync_channel::<LeaveFuture>(1);
    let (send_done, receive_done) = std::sync::mpsc::sync_channel(1);
    let origin = std::thread::spawn(move || {
        let _runtime = runtime.enter();
        let mut leave: LeaveFuture = Box::pin(
            async move {
                server.leave_room_locked(&player, false).await;
            }
            .with_subscriber(dispatch.clone()),
        );
        let waker = futures_util::task::noop_waker();
        let mut context = std::task::Context::from_waker(&waker);
        assert!(leave.as_mut().poll(&mut context).is_pending());
        send_future
            .send(leave)
            .expect("transfer the paused departure");
        receive_done
            .recv_timeout(Duration::from_secs(5))
            .expect("other worker canceled departure");
        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!("unrelated connection event");
            assert!(
                tracing::Span::current().id().is_none(),
                "cancellation left a stale span on the origin worker"
            );
        });
    });
    let cancellation = std::thread::spawn(move || {
        let _runtime = other_runtime.enter();
        let leave = receive_future
            .recv_timeout(Duration::from_secs(5))
            .expect("receive paused departure");
        drop(leave);
        send_done.send(()).expect("report cancellation");
    });
    cancellation.join().expect("cancellation worker");
    origin
        .join()
        .expect("origin worker must still be able to log");
    drop(gate);
}
