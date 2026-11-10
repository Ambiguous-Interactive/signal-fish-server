use std::collections::BTreeMap;
use std::future::Future;

use tokio::sync::{watch, Mutex};
use tokio::task::{Id, JoinError, JoinSet};

use crate::protocol::{PlayerId, RoomId};

type Candidate = (RoomId, PlayerId);

pub(super) const CONCURRENCY: usize = 8;
const SWEEP_WAIT: std::time::Duration = std::time::Duration::from_millis(250);

/// Keep admitted cleanup jobs alive across observation timeouts and caller cancellation.
#[derive(Default)]
pub(super) struct MaintenanceWorkers {
    state: Mutex<WorkerState>,
}

#[derive(Default)]
struct WorkerState {
    tasks: JoinSet<usize>,
    candidates: BTreeMap<Id, Candidate>,
    cursor: Option<Candidate>,
}

impl WorkerState {
    /// Retire one finished worker and count only confirmed cleanup.
    fn complete(&mut self, result: Result<(Id, usize), JoinError>) -> usize {
        match result {
            Ok((id, cleaned)) => {
                self.candidates.remove(&id);
                cleaned
            }
            Err(error) => {
                let candidate = self.candidates.remove(&error.id());
                tracing::error!(?candidate, %error, "Maintenance candidate failed; retaining it for retry");
                0
            }
        }
    }
}

impl MaintenanceWorkers {
    /// Observe drain only for queues that stop new admissions during shutdown.
    async fn wait_for_drain(draining: &mut Option<watch::Receiver<bool>>) {
        match draining {
            Some(receiver) => {
                let _ = receiver.changed().await;
            }
            None => std::future::pending().await,
        }
    }

    /// Give candidate discovery and all worker waits one shared deadline.
    pub(super) fn deadline() -> Option<tokio::time::Instant> {
        crate::deadline::after(tokio::time::Instant::now(), SWEEP_WAIT)
    }

    /// Stop a read-only candidate scan when the shared observation budget expires.
    pub(super) async fn observe<T>(
        deadline: Option<tokio::time::Instant>,
        read: impl Future<Output = T>,
    ) -> Option<T> {
        tokio::select! {
            biased;
            _ = crate::deadline::wait_until(deadline) => None,
            result = read => Some(result),
        }
    }

    /// Count one confirmed cleanup per successful candidate.
    pub(super) async fn run<F, Fut>(
        &self,
        candidates: Vec<Candidate>,
        deadline: Option<tokio::time::Instant>,
        draining: Option<watch::Receiver<bool>>,
        start: F,
    ) -> usize
    where
        F: Fn(Candidate) -> Fut,
        Fut: Future<Output = bool> + Send + 'static,
    {
        self.run_counted(candidates, deadline, draining, |candidate| {
            let work = start(candidate);
            async move { usize::from(work.await) }
        })
        .await
    }

    /// Rotate scans, bound concurrency, and share one wait across the entire batch.
    /// Completed counts include previously admitted jobs observed by this sweep.
    pub(super) async fn run_counted<F, Fut>(
        &self,
        mut candidates: Vec<Candidate>,
        deadline: Option<tokio::time::Instant>,
        mut draining: Option<watch::Receiver<bool>>,
        start: F,
    ) -> usize
    where
        F: Fn(Candidate) -> Fut,
        Fut: Future<Output = usize> + Send + 'static,
    {
        let mut state = tokio::select! {
            biased;
            _ = Self::wait_for_drain(&mut draining) => return 0,
            state = self.state.lock() => state,
            _ = crate::deadline::wait_until(deadline) => return 0,
        };
        candidates.sort_unstable();
        candidates.dedup();
        if let Some(cursor) = state.cursor {
            let offset = candidates.partition_point(|candidate| *candidate <= cursor);
            candidates.rotate_left(offset);
        }
        let mut candidates = candidates.into_iter();
        let mut cleaned = 0_usize;
        loop {
            while let Some(result) = state.tasks.try_join_next_with_id() {
                cleaned = cleaned.saturating_add(state.complete(result));
            }
            if draining.as_ref().is_some_and(|receiver| *receiver.borrow())
                || deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
            {
                return cleaned;
            }
            while state.tasks.len() < CONCURRENCY {
                let Some(candidate) = candidates.next() else {
                    break;
                };
                if state
                    .candidates
                    .values()
                    .any(|running| *running == candidate)
                {
                    continue;
                }
                let task = state.tasks.spawn(start(candidate));
                state.candidates.insert(task.id(), candidate);
                state.cursor = Some(candidate);
            }
            if state.tasks.is_empty() {
                return cleaned;
            }
            tokio::select! {
                biased;
                _ = Self::wait_for_drain(&mut draining) => return cleaned,
                result = state.tasks.join_next_with_id() => {
                    if let Some(result) = result {
                        cleaned = cleaned.saturating_add(state.complete(result));
                    }
                }
                _ = crate::deadline::wait_until(deadline) => return cleaned,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Semaphore;
    use tokio::time::{timeout, Duration};

    /// Build stable room/player identities for bounded-worker histories.
    fn candidates(count: u128) -> Vec<Candidate> {
        (1..=count)
            .map(|id| (RoomId::from_u128(id), PlayerId::from_u128(id)))
            .collect()
    }

    /// Retain admitted work when its caller ends and refuse duplicate admission.
    #[tokio::test(start_paused = true)]
    async fn canceled_observer_retains_bounded_jobs_without_duplicate_admission() {
        let workers = Arc::new(MaintenanceWorkers::default());
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Semaphore::new(0));
        let (drain, draining) = watch::channel(false);
        let observer = {
            let workers = workers.clone();
            let started = started.clone();
            let finished = finished.clone();
            let release = release.clone();
            tokio::spawn(async move {
                workers
                    .run(
                        candidates(16),
                        MaintenanceWorkers::deadline(),
                        Some(draining),
                        |_| {
                            let started = started.clone();
                            let finished = finished.clone();
                            let release = release.clone();
                            async move {
                                started.fetch_add(1, Ordering::SeqCst);
                                release.acquire().await.unwrap().forget();
                                finished.fetch_add(1, Ordering::SeqCst);
                                true
                            }
                        },
                    )
                    .await
            })
        };
        timeout(Duration::from_secs(1), async {
            while started.load(Ordering::SeqCst) < CONCURRENCY {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        observer.abort();
        assert!(observer.await.unwrap_err().is_cancelled());
        let repeated = workers
            .run(
                candidates(16),
                MaintenanceWorkers::deadline(),
                Some(drain.subscribe()),
                |_| async {
                    panic!("an observer must not duplicate or exceed the retained worker limit")
                },
            )
            .await;
        assert_eq!(repeated, 0);
        assert_eq!(started.load(Ordering::SeqCst), CONCURRENCY);
        assert_eq!(finished.load(Ordering::SeqCst), 0);
        release.add_permits(CONCURRENCY);
        assert_eq!(
            workers
                .run(
                    vec![],
                    MaintenanceWorkers::deadline(),
                    Some(drain.subscribe()),
                    |_| async { panic!("there are no new candidates") }
                )
                .await,
            CONCURRENCY
        );
        assert_eq!(finished.load(Ordering::SeqCst), CONCURRENCY);
        assert!(workers.state.lock().await.candidates.is_empty());
    }

    /// Give later candidates a turn before early busy rooms repeat.
    #[tokio::test(start_paused = true)]
    async fn bounded_scans_rotate_past_busy_candidates() {
        let workers = MaintenanceWorkers::default();
        let seen = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
        let (_drain, draining) = watch::channel(false);
        for _ in 0..2 {
            let seen = seen.clone();
            let cleaned = workers
                .run(
                    candidates(40),
                    MaintenanceWorkers::deadline(),
                    Some(draining.clone()),
                    move |candidate| {
                        let seen = seen.clone();
                        async move {
                            seen.lock().unwrap().insert(candidate);
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            false
                        }
                    },
                )
                .await;
            assert_eq!(cleaned, 0);
        }
        assert_eq!(
            seen.lock().unwrap().len(),
            40,
            "later rooms must receive an attempt before old busy rooms repeat"
        );
    }

    /// Release a panicked worker slot without consuming its retry obligation.
    #[tokio::test(start_paused = true)]
    async fn panic_releases_candidate_for_a_later_retry() {
        let workers = MaintenanceWorkers::default();
        let (_drain, draining) = watch::channel(false);
        let cleaned = workers
            .run(
                candidates(1),
                MaintenanceWorkers::deadline(),
                Some(draining.clone()),
                |_| async { panic!("injected candidate panic") },
            )
            .await;
        assert_eq!(cleaned, 0);
        assert!(workers.state.lock().await.candidates.is_empty());
        assert_eq!(
            workers
                .run(
                    candidates(1),
                    MaintenanceWorkers::deadline(),
                    Some(draining),
                    |_| async { true }
                )
                .await,
            1
        );
    }

    /// Stop new drain admissions while already admitted jobs finish.
    #[tokio::test(start_paused = true)]
    async fn drain_stops_new_jobs_and_preserves_already_admitted_work() {
        let workers = Arc::new(MaintenanceWorkers::default());
        let release = Arc::new(Semaphore::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(AtomicUsize::new(0));
        let (drain, draining) = watch::channel(false);
        let observer = {
            let workers = workers.clone();
            let release = release.clone();
            let started = started.clone();
            let finished = finished.clone();
            tokio::spawn(async move {
                workers
                    .run(
                        candidates(16),
                        MaintenanceWorkers::deadline(),
                        Some(draining),
                        |_| {
                            let release = release.clone();
                            let started = started.clone();
                            let finished = finished.clone();
                            async move {
                                started.fetch_add(1, Ordering::SeqCst);
                                release.acquire().await.unwrap().forget();
                                finished.fetch_add(1, Ordering::SeqCst);
                                true
                            }
                        },
                    )
                    .await
            })
        };
        timeout(Duration::from_secs(1), async {
            while started.load(Ordering::SeqCst) < CONCURRENCY {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drain.send(true).unwrap();
        assert_eq!(observer.await.unwrap(), 0);
        assert_eq!(started.load(Ordering::SeqCst), CONCURRENCY);
        release.add_permits(CONCURRENCY);
        timeout(Duration::from_secs(1), async {
            while finished.load(Ordering::SeqCst) < CONCURRENCY {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            workers
                .run(
                    candidates(16),
                    MaintenanceWorkers::deadline(),
                    Some(drain.subscribe()),
                    |_| async { panic!("drain must not admit new cleanup") }
                )
                .await,
            CONCURRENCY
        );
    }
}
