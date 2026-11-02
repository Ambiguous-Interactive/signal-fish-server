//! Main-thread runner for process-global allocation measurements.

/// Keep harness output and bookkeeping on the measuring thread, outside each region.
/// This remains sequential even when Cargo passes a larger test-thread count.
pub fn run(name: &'static str, test: fn()) {
    let mut arguments = libtest_mimic::Arguments::from_args();
    arguments.test_threads = Some(1);
    let main_thread = std::thread::current().id();
    let trial = libtest_mimic::Trial::test(name, move || {
        assert_eq!(std::thread::current().id(), main_thread);
        test();
        Ok(())
    });
    libtest_mimic::run(&arguments, vec![trial]).exit();
}
