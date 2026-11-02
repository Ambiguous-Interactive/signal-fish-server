//! Controls for the process-global allocation fixtures' runner.

mod allocation_harness;

/// The isolation guard must reject a harness that dispatches its body to a worker.
#[test]
fn allocation_trial_rejects_worker_execution() {
    let arguments = libtest_mimic::Arguments {
        test_threads: Some(2),
        ..Default::default()
    };
    let trial = allocation_harness::trial("worker_negative_control", || {});
    let outcome = libtest_mimic::run(&arguments, vec![trial]);
    assert_eq!(outcome.num_failed, 1);
    assert_eq!(outcome.num_passed, 0);
}

/// Discovery, exact filtering and ignored diagnostics must keep their original names.
#[test]
fn allocation_trial_preserves_filtering_and_ignored_diagnostics() {
    let arguments = libtest_mimic::Arguments {
        test_threads: Some(1),
        filter: Some("selected".into()),
        exact: true,
        ..Default::default()
    };
    let outcome = libtest_mimic::run(
        &arguments,
        vec![
            allocation_harness::trial("other", || panic!("filtered history ran")),
            allocation_harness::trial("selected", || {}),
            allocation_harness::trial("diagnostic", || panic!("ignored diagnostic ran"))
                .with_ignored_flag(true),
        ],
    );
    assert_eq!(outcome.num_passed, 1);
    assert_eq!(outcome.num_filtered_out, 2);
    assert_eq!(outcome.num_failed, 0);
    let arguments = libtest_mimic::Arguments {
        test_threads: Some(1),
        ignored: true,
        ..Default::default()
    };
    let outcome = libtest_mimic::run(
        &arguments,
        vec![
            allocation_harness::trial("normal", || panic!("non-ignored history ran")),
            allocation_harness::trial("diagnostic", || {}).with_ignored_flag(true),
        ],
    );
    assert_eq!(outcome.num_passed, 1);
    assert_eq!(outcome.num_filtered_out, 1);
    assert_eq!(outcome.num_failed, 0);
}
