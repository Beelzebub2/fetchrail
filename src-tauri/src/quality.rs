//! Optional schema 3. Typed, source-free summaries with bounded acknowledged delivery.
use crate::health::{Cause, Phase, Report as HealthReport};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Mutex};
use tokio_util::sync::CancellationToken;
mod delivery;
pub use delivery::DeliveryStatus;
pub const SCHEMA: u32 = 3;
pub const PURPOSE: &str = "product-quality-v3";
// Owner-authorized rollout after processing review; this is no compliance certification.
pub const REMOTE_RELEASE_APPROVED: bool = true;
pub const NOTICE_REVISION: &str = "reliability-v3-20261021";
pub const BOUNDS_MS: [u64; 7] = [10, 100, 1000, 10_000, 60_000, 300_000, 1_800_000];
pub const E2E_BOUNDS_MS: [u64; 14] = [
    1000, 5000, 10_000, 30_000, 60_000, 90_000, 120_000, 150_000, 180_000, 210_000, 240_000,
    300_000, 600_000, 1_800_000,
];
pub const DELIVERY_BOUNDS_MBPS: [u64; 14] =
    [1, 5, 10, 20, 40, 60, 80, 100, 110, 120, 140, 180, 250, 500];
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
            schema: SCHEMA,
            purpose: PURPOSE.into(),
            decided_at: chrono::Utc::now().to_rfc3339(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            notice_revision: NOTICE_REVISION.into(),
        }
    }
    pub fn active(&self) -> bool {
        self.enabled
            && self.decided
            && self.schema == SCHEMA
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
    grant: String,
    counters: BTreeMap<String, u32>,
    histograms: BTreeMap<String, Vec<u32>>,
    cancel: CancellationToken,
}
#[derive(Default)]
pub struct Quality(Mutex<State>, Mutex<delivery::Delivery>);
pub struct Sample {
    epoch: u64,
    registered: bool,
    terminal: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    schema: u32,
    purpose: String,
    version: String,
    os: String,
    arch: String,
    #[serde(rename = "batchId")]
    batch_id: String,
    provenance: String,
    counters: BTreeMap<String, u32>,
    histograms: BTreeMap<String, Vec<u32>>,
}
impl Report {
    pub(crate) fn batch_id(&self) -> &str {
        &self.batch_id
    }
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
        if enabled != s.enabled || enabled && s.grant != consent.decided_at {
            s.cancel.cancel();
            s.cancel = CancellationToken::new();
            s.epoch = s.epoch.wrapping_add(1);
            s.counters.clear();
            s.histograms.clear();
            s.enabled = enabled;
            s.grant = if enabled {
                consent.decided_at.clone()
            } else {
                String::new()
            };
            self.1.lock().unwrap().configure(&s.grant);
        } else if !enabled {
            // Retrying an off choice retries deletion after a temporary storage failure.
            self.1.lock().unwrap().configure("");
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
        // Configuration observation survives local policy-ring eviction. It is not
        // proof the limiter constrained body bytes, and does not require a rate sample.
        if matches!(outcome, Outcome::HttpDurable)
            && (health.configured_cap_observed
                || health.file_limit_bps.is_some_and(|n| n > 0)
                || health.global_limit_bps.is_some_and(|n| n > 0))
        {
            counter(&mut s, "delivery_cap_observed", 1);
        }
        if let Some(ms) = health
            .durable_completed_ms
            .filter(|_| health.observed_from_accept && matches!(outcome, Outcome::HttpDurable))
        {
            histogram(&mut s, &format!("http_e2e_{}_ms_v2", size_band(total)), ms);
            // A cumulative clock survives local lifecycle-ring eviction; no reconstruction
            // from the retained tail or resumed output bytes is permitted here.
            let active_ms = health.transfer_activity_ms;
            if let Some(active_ms) = active_ms.filter(|ms| *ms > 0) {
                let bps = (u128::from(health.received_wire_bytes) * 1000 / u128::from(active_ms))
                    .min(u128::from(u64::MAX)) as u64;
                histogram(
                    &mut s,
                    &format!("http_delivery_{}_mbps_v1", size_band(total)),
                    bps,
                );
            } else {
                counter(&mut s, "throughput_unobserved", 1);
            }
        } else if matches!(outcome, Outcome::HttpDurable) {
            counter(&mut s, "e2e_unobserved", 1);
            counter(&mut s, "throughput_unobserved", 1);
        }
    }
    pub fn preview(&self) -> Option<Report> {
        let s = self.0.lock().unwrap();
        s.enabled
            .then(|| report(s.counters.clone(), s.histograms.clone()))
    }
    #[cfg(test)]
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
fn report(counters: BTreeMap<String, u32>, histograms: BTreeMap<String, Vec<u32>>) -> Report {
    Report {
        schema: SCHEMA,
        purpose: PURPOSE.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        os: match std::env::consts::OS {
            "windows" => "windows",
            "linux" => "linux",
            _ => "other",
        }
        .into(),
        arch: match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            _ => "other",
        }
        .into(),
        batch_id: uuid::Uuid::new_v4().simple().to_string(),
        // This is a declaration, not proof that an anonymous sender is genuine.
        provenance: if cfg!(feature = "test-tools") {
            "controlled"
        } else {
            "production"
        }
        .into(),
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
    let bounds = histogram_bounds(key);
    let bucket = bounds
        .iter()
        .position(|&bound| ms <= bound)
        .unwrap_or(bounds.len());
    let count = &mut s
        .histograms
        .entry(key.into())
        .or_insert_with(|| vec![0; bounds.len() + 1])[bucket];
    if *count == MAX {
        s.counters.insert("clipped".into(), 1);
    } else {
        *count += 1;
    }
}
fn histogram_bounds(key: &str) -> Vec<u64> {
    if key.starts_with("http_delivery_") {
        DELIVERY_BOUNDS_MBPS.iter().map(|n| n * 1_000_000).collect()
    } else if key.starts_with("http_e2e_") && key.ends_with("_ms_v2") {
        E2E_BOUNDS_MS.to_vec()
    } else {
        BOUNDS_MS.to_vec()
    }
}
fn size_band(total: Option<u64>) -> &'static str {
    match total {
        None => "unknown",
        Some(n) if n < 10 * 1024 * 1024 => "under10m",
        Some(n) if n < 1024 * 1024 * 1024 => "10m1g",
        Some(n) if n < 4 * 1024 * 1024 * 1024 => "1g4g",
        Some(n) if n < 16 * 1024 * 1024 * 1024 => "4g16g",
        Some(n) if n < 64 * 1024 * 1024 * 1024 => "16g64g",
        _ => "over64g",
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
        // PrefixHash, DispatchLock and CatalogLock remain local-only evidence;
        // the remote worker vocabulary is the separately disclosed fixed list.
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
        let mut v2 = Consent::choice(true);
        v2.schema = 2;
        v2.purpose = "product-quality-v2".into();
        quality.configure(&v2);
        assert!(
            quality.prepare().is_none(),
            "A v2 grant does not authorize persistence or finer metrics"
        );
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
            for size in [
                "unknown", "under10m", "10m1g", "1g4g", "4g16g", "16g64g", "over64g",
            ] {
                for (suffix, bounds) in [
                    ("ms_v2", E2E_BOUNDS_MS.to_vec()),
                    (
                        "mbps_v1",
                        DELIVERY_BOUNDS_MBPS.iter().map(|n| n * 1_000_000).collect(),
                    ),
                ] {
                    let prefix = if suffix == "ms_v2" {
                        "http_e2e"
                    } else {
                        "http_delivery"
                    };
                    let key = format!("{prefix}_{size}_{suffix}");
                    s.histograms.insert(key, vec![MAX; bounds.len() + 1]);
                }
            }
        }
        let json = serde_json::to_vec(&quality.take_report().unwrap().0).unwrap();
        assert!(json.len() < 8192);
        assert_eq!(BOUNDS_MS, [10, 100, 1000, 10000, 60000, 300000, 1800000]);
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../fixtures/quality-v3.json")).unwrap();
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
    #[test]
    fn fine_duration_and_delivery_buckets_use_observed_body_activity_and_size() {
        let quality = Quality::default();
        quality.configure(&Consent::choice(true));
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        let mut health = crate::health::Health::default().snapshot(false);
        health.observed_from_accept = true;
        health.durable_completed_ms = Some(175_000);
        health.transfer_activity_ms = Some(160_000);
        health.received_wire_bytes = 16 * 1024 * 1024 * 1024;
        health.file_limit_bps = Some(110_000_000);
        quality.outcome(
            &mut sample,
            false,
            Outcome::HttpDurable,
            Some(20 * 1024 * 1024 * 1024),
            &health,
        );
        let report = quality.take_report().unwrap().0;
        assert_eq!(report.histograms["http_e2e_16g64g_ms_v2"][8], 1); // <=180s
        assert_eq!(report.histograms["http_delivery_16g64g_mbps_v1"][8], 1); // <=110 decimal MB/s
        assert_eq!(report.counters["delivery_cap_observed"], 1);
        assert!(!report.counters.contains_key("throughput_unobserved"));
        assert!(!report.histograms.contains_key("http_e2e_large_ms_v1"));
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        health.observed_from_accept = false;
        quality.outcome(
            &mut sample,
            false,
            Outcome::HttpDurable,
            Some(20 * 1024 * 1024 * 1024),
            &health,
        );
        let report = quality.take_report().unwrap().0;
        assert_eq!(report.counters["e2e_unobserved"], 1);
        assert_eq!(report.counters["throughput_unobserved"], 1);
        assert!(!report
            .histograms
            .keys()
            .any(|key| key.starts_with("http_delivery_") || key.starts_with("http_e2e_")));
        assert_eq!(size_band(Some(64 * 1024 * 1024 * 1024)), "over64g");
    }
    #[test]
    fn real_health_policy_caps_survive_eviction_and_feed_quality_until_clear() {
        use crate::health::{EffectivePolicy, Health};
        let quality = Quality::default();
        quality.configure(&Consent::choice(true));
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        let health = Health::default();
        health.accepted();
        health.policy(EffectivePolicy {
            connections: 8,
            file_limit_bps: 1_000_000,
            ..Default::default()
        });
        let initial = health.snapshot(false);
        assert_eq!(initial.file_limit_bps, Some(1_000_000));
        assert_eq!(initial.global_limit_bps, Some(0));
        assert!(initial.configured_cap_observed);
        health.runtime_policy(8, 0, 2_000_000, false);
        assert_eq!(health.snapshot(false).global_limit_bps, Some(2_000_000));
        for connections in 1..=40 {
            health.runtime_policy(1 + connections % 32, 0, 0, false);
        }
        health.completed(1024);
        let observed = health.snapshot(false);
        assert_eq!(observed.policy_changes.len(), 32);
        assert!(
            observed
                .policy_changes
                .iter()
                .all(|change| change.policy.file_limit_bps == 0
                    && change.policy.global_limit_bps == 0)
        );
        assert_eq!(observed.file_limit_bps, Some(0));
        assert_eq!(observed.global_limit_bps, Some(0));
        assert!(observed.configured_cap_observed);
        quality.outcome(
            &mut sample,
            false,
            Outcome::HttpDurable,
            Some(1024),
            &observed,
        );
        assert_eq!(
            quality.take_report().unwrap().0.counters["delivery_cap_observed"],
            1
        );
        let mut old = serde_json::to_value(&observed).unwrap();
        old.as_object_mut().unwrap().remove("configuredCapObserved");
        assert!(
            !serde_json::from_value::<crate::health::Report>(old)
                .unwrap()
                .configured_cap_observed
        );
        health.clear();
        let cleared = health.snapshot(false);
        assert!(!cleared.configured_cap_observed);
        assert_eq!(cleared.file_limit_bps, None);
        assert_eq!(cleared.global_limit_bps, None);
        health.runtime_policy(1, 0, 3_000_000, false);
        assert!(
            health.snapshot(false).configured_cap_observed,
            "A cap observation does not require a retained initial policy"
        );
    }
}
