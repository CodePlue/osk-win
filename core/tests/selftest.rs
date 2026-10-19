//! The full self-test as a single cargo test, mirroring `osk --selftest`.

#[test]
fn selftest() {
    let report = oskwin_core::selftest::run();
    assert!(
        report.ok(),
        "{} of {} cases failed: {}",
        report.failures.len(),
        report.total(),
        report.failures.join("; ")
    );
}
