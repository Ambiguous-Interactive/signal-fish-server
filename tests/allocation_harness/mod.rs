//! Main-thread runner for process-global allocation measurements.

/// Run one history without concurrent harness setup or result collection.
#[allow(dead_code)]
pub fn run(name: &'static str, test: fn()) {
    run_trials(vec![trial(name, test)]);
}

/// Keep every discovered history on the thread which built its trial.
pub fn trial(name: &'static str, test: fn()) -> libtest_mimic::Trial {
    let main_thread = std::thread::current().id();
    libtest_mimic::Trial::test(name, move || {
        assert_eq!(std::thread::current().id(), main_thread);
        test();
        Ok(())
    })
}

/// Preserve filtering and ignored diagnostics while refusing parallel execution.
pub fn run_trials(trials: Vec<libtest_mimic::Trial>) {
    let mut arguments = libtest_mimic::Arguments::from_args();
    arguments.test_threads = Some(1);
    libtest_mimic::run(&arguments, trials).exit();
}

/// Poll each asynchronous history on the measuring thread with its original runtime.
#[allow(dead_code)]
pub fn block_on(future: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future);
}

/// Initialize the current-thread runtime's four task-registry shards before measurement.
#[allow(dead_code)]
pub async fn warm_current_thread_runtime() {
    tokio::task::yield_now().await;
    for _ in 0..4 {
        tokio::spawn(std::future::ready(())).await.unwrap();
    }
}
