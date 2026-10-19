//! Optional schema 2. This boundary accepts typed, source-free observations only.
use crate::health::{Cause, Phase, Report as HealthReport};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Mutex};
use tokio_util::sync::CancellationToken;
pub const PURPOSE: &str = "product-quality-v2";
// Requires the documented legal/configuration review and a separately authorized release.
pub const REMOTE_RELEASE_APPROVED: bool = false;
pub const NOTICE_REVISION: &str = "reliability-draft-20261019";
pub const BOUNDS_MS: [u64; 7] = [10, 100, 1000, 10_000, 60_000, 300_000, 1_800_000];
const MAX: u32 = 1000;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Consent {
    pub enabled: bool,
    pub decided: bool,
    pub schema: u32,
    pub purpose: String,
    pub decided_at: String,
    pub app_version: String,
    pub notice_revision: String,
}
impl Consent {
    pub fn choice(enabled: bool) -> Self {
        Self {
            enabled,
            decided: true,
            schema: 2,
            purpose: PURPOSE.into(),
            decided_at: chrono::Utc::now().to_rfc3339(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            notice_revision: NOTICE_REVISION.into(),
        }
    }
    pub fn active(&self) -> bool {
        self.enabled
            && self.decided
            && self.schema == 2
            && self.purpose == PURPOSE
            && !self.decided_at.is_empty()
            && !self.app_version.is_empty()
            && self.notice_revision == NOTICE_REVISION
    }
}
#[derive(Default)]
struct State {
    enabled: bool,
    epoch: u64,
    counters: BTreeMap<String, u32>,
    histograms: BTreeMap<String, [u32; 8]>,
    cancel: CancellationToken,
}
#[derive(Default)]
pub struct Quality(Mutex<State>);
pub struct Sample {
    epoch: u64,
    registered: bool,
    terminal: bool,
}
#[derive(Clone, Serialize)]
pub struct Report {
    schema: u32,
    purpose: &'static str,
    version: &'static str,
    os: &'static str,
    arch: &'static str,
    counters: BTreeMap<String, u32>,
    histograms: BTreeMap<String, [u32; 8]>,
}
#[derive(Clone, Copy)]
pub enum Outcome {
    HttpDurable,
    TorrentSelectedReady,
    Failed,
    Cancelled,
}
impl Quality {
    pub fn configure(&self, consent: &Consent) {
        let mut s = self.0.lock().unwrap();
        let enabled = consent.active();
        if enabled != s.enabled {
            s.cancel.cancel();
            s.cancel = CancellationToken::new();
            s.epoch = s.epoch.wrapping_add(1);
            s.counters.clear();
            s.histograms.clear();
            s.enabled = enabled;
        }
    }
    pub fn prepare(&self) -> Option<Sample> {
        let s = self.0.lock().unwrap();
        s.enabled.then_some(Sample {
            epoch: s.epoch,
            registered: false,
            terminal: false,
        })
    }
    pub fn eligible(&self, sample: &Sample) -> bool {
        let s = self.0.lock().unwrap();
        s.enabled && s.epoch == sample.epoch && sample.registered && !sample.terminal
    }
    pub fn added(&self, sample: &mut Sample, torrent: bool) {
        let mut s = self.0.lock().unwrap();
        if !s.enabled || s.epoch != sample.epoch || sample.registered {
            return;
        }
        sample.registered = true;
        counter(
            &mut s,
            if torrent {
                "torrent_accepted"
            } else {
                "http_accepted"
            },
            1,
        );
    }
    pub fn outcome(
        &self,
        sample: &mut Sample,
        torrent: bool,
        outcome: Outcome,
        total: Option<u64>,
        health: &HealthReport,
    ) {
        let mut s = self.0.lock().unwrap();
        if !s.enabled
            || s.epoch != sample.epoch
            || !sample.registered
            || sample.terminal
            || health.context != crate::health::Context::Transfer
        {
            return;
        }
        if matches!(outcome, Outcome::HttpDurable)
            && (torrent || health.durable_completed_ms.is_none())
        {
            return;
        }
        let key = match (torrent, outcome) {
            (false, Outcome::HttpDurable) => "http_verified_durable",
            (true, Outcome::TorrentSelectedReady) => "torrent_selected_ready",
            (false, Outcome::Failed) => "http_failed",
            (true, Outcome::Failed) => "torrent_failed",
            (false, Outcome::Cancelled) => "http_cancelled",
            (true, Outcome::Cancelled) => "torrent_cancelled",
            _ => return,
        };
        sample.terminal = true;
        counter(&mut s, key, 1);
        counter(
            &mut s,
            match total {
                None => "size_unknown",
                Some(n) if n < 10 * 1024 * 1024 => "size_small",
                Some(n) if n < 1024 * 1024 * 1024 => "size_medium",
                _ => "size_large",
            },
            1,
        );
        if health.receive_stalls > 0 {
            counter(&mut s, "receive_stalls", health.receive_stalls);
            histogram(&mut s, "stall_total_ms_v1", health.receive_stalled_ms);
        }
        for &(protocol, n) in &health.protocols {
            counter(
                &mut s,
                match protocol {
                    crate::health::Protocol::Http1 => "http1_responses",
                    crate::health::Protocol::Http2 => "http2_responses",
                    crate::health::Protocol::Other => "other_protocol_responses",
                },
                n,
            );
        }
        if let Some(torrent) = &health.torrent_health {
            for (key, n) in [
                ("torrent_hash_failures", torrent.hash_failures),
                ("torrent_file_errors", torrent.file_errors),
                ("torrent_engine_errors", torrent.engine_errors),
                ("torrent_metadata_failures", torrent.metadata_failures),
                ("torrent_checkpoint_errors", torrent.checkpoint_errors),
            ] {
                if n > 0 {
                    counter(&mut s, key, n);
                }
            }
        }
        for &(cause, n) in &health.causes {
            counter(&mut s, cause_key(cause), n);
        }
        // A persisted cancellation can precede worker drain; unfinished spans have no final duration.
        for stage in health.stages.iter().filter(|stage| stage.active == 0) {
            if let Some(key) = stage_key(stage.phase) {
                histogram(&mut s, key, stage.worker_ms);
            }
        }
        if let Some(ms) = health
            .durable_completed_ms
            .filter(|_| health.observed_from_accept && matches!(outcome, Outcome::HttpDurable))
        {
            histogram(
                &mut s,
                match total {
                    None => "http_e2e_unknown_ms_v1",
                    Some(n) if n < 10 * 1024 * 1024 => "http_e2e_small_ms_v1",
                    Some(n) if n < 1024 * 1024 * 1024 => "http_e2e_medium_ms_v1",
                    _ => "http_e2e_large_ms_v1",
                },
                ms,
            );
        } else if matches!(outcome, Outcome::HttpDurable) {
            counter(&mut s, "e2e_unobserved", 1);
        }
    }
    pub fn preview(&self) -> Option<Report> {
        let s = self.0.lock().unwrap();
        s.enabled
            .then(|| report(s.counters.clone(), s.histograms.clone()))
    }
    pub fn take_report(&self) -> Option<(Report, CancellationToken)> {
        let mut s = self.0.lock().unwrap();
        if !s.enabled || s.counters.is_empty() {
            return None;
        }
        Some((
            report(
                std::mem::take(&mut s.counters),
                std::mem::take(&mut s.histograms),
            ),
            s.cancel.clone(),
        ))
    }
}
fn report(counters: BTreeMap<String, u32>, histograms: BTreeMap<String, [u32; 8]>) -> Report {
    Report {
        schema: 2,
        purpose: PURPOSE,
        version: env!("CARGO_PKG_VERSION"),
        os: match std::env::consts::OS {
            "windows" => "windows",
            "linux" => "linux",
            _ => "other",
        },
        arch: match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => "other",
        },
        counters,
        histograms,
    }
}
fn counter(s: &mut State, key: &str, n: u64) {
    let value = s.counters.entry(key.into()).or_default();
    let clipped = n > u64::from(MAX.saturating_sub(*value));
    *value = (u64::from(*value).saturating_add(n)).min(MAX as u64) as u32;
    if clipped {
        s.counters.insert("clipped".into(), 1);
    }
}
fn histogram(s: &mut State, key: &str, ms: u64) {
    let bucket = BOUNDS_MS.iter().position(|&bound| ms <= bound).unwrap_or(7);
    let count = &mut s.histograms.entry(key.into()).or_default()[bucket];
    if *count == MAX {
        s.counters.insert("clipped".into(), 1);
    } else {
        *count += 1;
    }
}
pub fn cause_key(cause: Cause) -> &'static str {
    match cause {
        Cause::Timeout => "timeout",
        Cause::DnsOrConnect => "dns_or_connect",
        Cause::Request => "request",
        Cause::BodyIncomplete => "body_incomplete",
        Cause::SlowPeer => "slow_peer",
        Cause::Http429 => "http_429",
        Cause::Http503 => "http_503",
        Cause::Http4xx => "http_4xx",
        Cause::Http5xx => "http_5xx",
        Cause::RangeIgnored => "range_ignored",
        Cause::RangeInvalid => "range_invalid",
        Cause::SourceChanged => "source_changed",
        Cause::Disk => "disk",
        Cause::Integrity => "integrity",
        Cause::Attachment => "attachment",
        Cause::PublishConflict => "publish_conflict",
        Cause::Catalog => "catalog",
        Cause::Cancelled => "attempt_cancelled",
        Cause::Unknown => "unknown",
    }
}
pub fn stage_key(phase: Phase) -> Option<&'static str> {
    Some(match phase {
        Phase::Probe => "probe_worker_ms_v1",
        Phase::Origin => "origin_worker_ms_v1",
        Phase::Global => "global_worker_ms_v1",
        Phase::Headers => "headers_worker_ms_v1",
        Phase::Body => "body_wait_worker_ms_v1",
        Phase::Limiter => "limiter_worker_ms_v1",
        Phase::Write => "write_worker_ms_v1",
        Phase::Flush => "flush_worker_ms_v1",
        Phase::Sync => "sync_worker_ms_v1",
        Phase::Checkpoint => "checkpoint_worker_ms_v1",
        Phase::Retry => "retry_worker_ms_v1",
        Phase::Assembly => "assembly_worker_ms_v1",
        Phase::Attachment => "attachment_worker_ms_v1",
        Phase::Verify => "verify_worker_ms_v1",
        Phase::Journal => "journal_worker_ms_v1",
        Phase::Publish => "publish_worker_ms_v1",
        Phase::Catalog => "catalog_worker_ms_v1",
        Phase::ScanOverlap => "scan_overlap_worker_ms_v1",
        _ => return None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_does_not_turn_an_active_stage_into_a_zero_duration_sample() {
        let quality = Quality::default();
        quality.configure(&Consent::choice(true));
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        let health = crate::health::Health::default();
        let _body = health.guard(Phase::Body);
        quality.outcome(
            &mut sample,
            false,
            Outcome::Cancelled,
            None,
            &health.snapshot(false),
        );
        let report = quality.take_report().unwrap().0;
        assert_eq!(report.counters["http_cancelled"], 1);
        assert!(!report.histograms.contains_key("body_wait_worker_ms_v1"));
    }
    #[test]
    fn pre_admission_evidence_never_becomes_a_transfer_outcome() {
        let quality = Quality::default();
        quality.configure(&Consent::choice(true));
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        let mut report = crate::health::Health::default().snapshot(false);
        report.context = crate::health::Context::BrowserVerification;
        quality.outcome(&mut sample, false, Outcome::Failed, None, &report);
        let report = quality.take_report().unwrap().0;
        assert_eq!(report.counters.len(), 1);
        assert_eq!(report.counters["http_accepted"], 1);
    }
    #[test]
    fn randomized_lifecycle_counts_each_eligible_logical_transfer_at_most_once() {
        let quality = Quality::default();
        quality.configure(&Consent::choice(true));
        let health = crate::health::Health::default();
        health.accepted();
        health.completed(10);
        let report = health.snapshot(false);
        let mut random = 42u64;
        for _ in 0..200 {
            let mut sample = quality.prepare().unwrap();
            for _ in 0..32 {
                random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                match random >> 62 {
                    0 => quality.added(&mut sample, false),
                    1 => quality.outcome(&mut sample, false, Outcome::Failed, Some(10), &report),
                    2 => quality.outcome(&mut sample, false, Outcome::Cancelled, Some(10), &report),
                    _ => {
                        quality.outcome(&mut sample, false, Outcome::HttpDurable, Some(10), &report)
                    }
                }
            }
        }
        let payload = quality.take_report().unwrap().0;
        let terminals: u32 = ["http_verified_durable", "http_failed", "http_cancelled"]
            .iter()
            .map(|key| payload.counters.get(*key).copied().unwrap_or(0))
            .sum();
        assert_eq!(terminals, 200);
        assert_eq!(payload.counters["http_accepted"], 200);
    }
    #[test]
    fn independent_scope_epochs_no_backfill_or_duplicate_outcomes() {
        let quality = Quality::default();
        assert!(quality.prepare().is_none());
        let mut stale = Consent::choice(true);
        stale.purpose = "product-improvement-v1".into();
        quality.configure(&stale);
        assert!(quality.prepare().is_none());
        let mut stale_notice = Consent::choice(true);
        stale_notice.notice_revision = "reliability-draft-old".into();
        quality.configure(&stale_notice);
        assert!(quality.prepare().is_none());
        quality.configure(&Consent::choice(true));
        assert!(quality.take_report().is_none());
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        quality.added(&mut sample, false);
        assert!(quality.eligible(&sample));
        let (_, token) = quality.take_report().unwrap();
        quality.configure(&Consent::choice(false));
        assert!(token.is_cancelled());
        assert!(!quality.eligible(&sample));
        quality.configure(&Consent::choice(true));
        let health = crate::health::Health::default();
        health.completed(123);
        quality.outcome(
            &mut sample,
            false,
            Outcome::HttpDurable,
            Some(123),
            &health.snapshot(false),
        );
        assert!(quality.take_report().is_none());
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        for _ in 0..3 {
            quality.outcome(
                &mut sample,
                false,
                Outcome::HttpDurable,
                None,
                &health.snapshot(false),
            );
        }
        assert_eq!(
            quality.take_report().unwrap().0.counters["http_verified_durable"],
            1
        );
    }
    #[test]
    fn maximum_serializer_is_bounded_and_shared_golden_contract() {
        let quality = Quality::default();
        quality.configure(&Consent::choice(true));
        {
            let mut s = quality.0.lock().unwrap();
            for cause in crate::health::CAUSES {
                counter(&mut s, cause_key(cause), u64::MAX);
            }
            for phase in crate::health::PHASES {
                if let Some(key) = stage_key(phase) {
                    for ms in [1, 100, 1000, 10000, 60000, 300000, 1800000, u64::MAX] {
                        histogram(&mut s, key, ms);
                    }
                }
            }
        }
        let json = serde_json::to_vec(&quality.take_report().unwrap().0).unwrap();
        assert!(json.len() < 4096);
        assert_eq!(BOUNDS_MS, [10, 100, 1000, 10000, 60000, 300000, 1800000]);
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../fixtures/quality-v2.json")).unwrap();
        assert_eq!(fixture["purpose"], PURPOSE);
        let quality = Quality::default();
        quality.configure(&Consent::choice(true));
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        let health = crate::health::Health::default();
        health.accepted();
        health.completed(1);
        let mut observed = health.snapshot(false);
        observed.durable_completed_ms = Some(1000);
        observed.stages = vec![crate::health::Stage {
            phase: Phase::Catalog,
            calls: 1,
            worker_ms: 1,
            max_ms: 1,
            active: 0,
            active_elapsed_ms: 0,
        }];
        quality.outcome(&mut sample, false, Outcome::HttpDurable, Some(1), &observed);
        let actual = serde_json::to_value(quality.take_report().unwrap().0).unwrap();
        assert_eq!(actual["counters"], fixture["counters"]);
        assert_eq!(actual["histograms"], fixture["histograms"]);
    }
}
