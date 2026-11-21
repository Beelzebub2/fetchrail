use super::*;

const MIB: u64 = 1024 * 1024;

#[test]
fn responsive_body_with_slow_local_writes_or_checkpoints_is_not_a_slow_source() {
    // One MiB arrives promptly; the same window spends 39.9 seconds doing local
    // work. The former wall-only classifier blamed the response in both cases.
    for local_work in ["write", "checkpoint"] {
        let mut progress = SlowProgress::default();
        let mut old_best = 0.0;
        let mut old_previous = 0.0;
        for _ in 0..3 {
            assert!(crate::network::slow_progress(
                MIB,
                Duration::from_secs(40),
                &mut old_best,
                &mut old_previous,
                MIB,
            ));
            assert!(
                !progress.observe(
                    MIB,
                    Duration::from_secs(40),
                    Duration::from_millis(100),
                    MIB,
                ),
                "Local {local_work} delay must not cause range replacement"
            );
        }
        assert_eq!(
            progress.best_rate, 0.0,
            "Discarded work cannot train a peer rate"
        );
    }
}

#[test]
fn genuine_slow_body_windows_still_trigger_source_recovery() {
    let mut progress = SlowProgress::default();
    assert!(!progress.observe(
        16 * 1024,
        Duration::from_secs(10),
        Duration::from_millis(9990),
        MIB,
    ));
    assert!(progress.observe(
        16 * 1024,
        Duration::from_secs(10),
        Duration::from_millis(9990),
        MIB,
    ));
    assert!(!progress.observe(
        10 * MIB,
        Duration::from_secs(10),
        Duration::from_millis(9990),
        MIB,
    ));
}

#[test]
fn local_work_breaks_consecutive_slow_evidence_without_erasing_proven_rate() {
    let mut progress = SlowProgress::default();
    for _ in 0..2 {
        assert!(!progress.observe(
            10 * MIB,
            Duration::from_secs(10),
            Duration::from_millis(9990),
            MIB,
        ));
    }
    let proven = progress.best_rate;
    assert!(proven >= MIB as f64);
    let slow = |progress: &mut SlowProgress| {
        progress.observe(1024, Duration::from_secs(10), Duration::from_secs(10), MIB)
    };
    assert!(!slow(&mut progress));
    assert!(!progress.observe(
        MIB,
        Duration::from_secs(40),
        Duration::from_millis(100),
        MIB,
    ));
    assert_eq!(progress.best_rate, proven);
    assert_eq!(progress.previous_rate, 0.0);
    assert!(!slow(&mut progress));
    assert!(slow(&mut progress));
}

#[test]
fn no_progress_body_wait_is_not_discarded_as_local_work() {
    let mut progress = SlowProgress::default();
    assert!(!progress.observe(0, Duration::from_secs(10), Duration::from_secs(10), MIB));
    assert!(progress.observe(0, Duration::from_secs(10), Duration::from_secs(10), MIB));

    // The independent five-second peer/tail checks remain usable while stream
    // polling yields no chunks. Engine request read_timeout remains 30 seconds.
    let mut weak = 0;
    assert!(!crate::network::weak_peer(
        0,
        Duration::from_secs(5),
        MIB,
        &mut weak
    ));
    assert!(crate::network::weak_peer(
        0,
        Duration::from_secs(5),
        MIB,
        &mut weak
    ));
}
