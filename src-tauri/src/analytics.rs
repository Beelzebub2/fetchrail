//! Optional, first-party counters. No identifiers or transfer details leave this module.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Mutex};
use tokio_util::sync::CancellationToken;

pub const SCHEMA: u32 = 1;
pub const PURPOSE: &str = "product-improvement-v1";
const ENDPOINT: &str = "https://fetchrail.rrmtools.uk/api/analytics";
const MAX_COUNT: u32 = 1000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Consent {
    pub enabled: bool,
    pub decided: bool,
    pub schema: u32,
    pub purpose: String,
}
impl Consent {
    pub fn choice(enabled: bool) -> Self {
        Self {
            enabled,
            decided: true,
            schema: SCHEMA,
            purpose: PURPOSE.into(),
        }
    }
    pub fn active(&self) -> bool {
        self.enabled && self.decided && self.schema == SCHEMA && self.purpose == PURPOSE
    }
}

#[derive(Default)]
struct State {
    enabled: bool,
    epoch: u64,
    counters: BTreeMap<&'static str, u32>,
    cancel: CancellationToken,
}
pub struct Analytics(Mutex<State>);
pub struct Sample {
    epoch: u64,
    registered: bool,
    terminal: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    schema: u32,
    version: &'static str,
    os: &'static str,
    arch: &'static str,
    counters: BTreeMap<&'static str, u32>,
}

impl Analytics {
    pub fn new(consent: &Consent) -> Self {
        let analytics = Self(Mutex::new(State::default()));
        analytics.configure(consent);
        // An app launch is counted only when consent already exists. Never unique users.
        analytics.feature("launch");
        analytics
    }
    pub fn configure(&self, consent: &Consent) {
        let mut state = self.0.lock().unwrap();
        let enabled = consent.active();
        if state.enabled != enabled {
            state.cancel.cancel();
            state.cancel = CancellationToken::new();
            state.counters.clear();
            state.epoch = state.epoch.wrapping_add(1);
            state.enabled = enabled;
        }
    }
    fn increment(state: &mut State, metric: &'static str) {
        let value = state.counters.entry(metric).or_default();
        *value = value.saturating_add(1).min(MAX_COUNT);
    }
    pub fn feature(&self, metric: &'static str) {
        // Closed vocabulary even inside the native engine.
        if !matches!(
            metric,
            "launch"
                | "pause"
                | "resume"
                | "speed_limit"
                | "queue"
                | "schedule"
                | "browser_handoff"
                | "batch"
        ) {
            return;
        }
        let mut state = self.0.lock().unwrap();
        if state.enabled {
            Self::increment(&mut state, metric);
        }
    }
    pub fn prepare(&self) -> Option<Sample> {
        let state = self.0.lock().unwrap();
        state.enabled.then_some(Sample {
            epoch: state.epoch,
            registered: false,
            terminal: false,
        })
    }
    pub fn added(
        &self,
        sample: &mut Sample,
        torrent: bool,
        scheduled: bool,
        custom_queue: bool,
        browser: bool,
    ) {
        let mut state = self.0.lock().unwrap();
        if !state.enabled || state.epoch != sample.epoch || sample.registered {
            return;
        }
        sample.registered = true;
        Self::increment(
            &mut state,
            if torrent {
                "torrent_added"
            } else {
                "http_added"
            },
        );
        for (used, key) in [
            (scheduled, "schedule"),
            (custom_queue, "queue"),
            (browser, "browser_handoff"),
        ] {
            if used {
                Self::increment(&mut state, key);
            }
        }
    }
    pub fn outcome(&self, sample: &mut Sample, torrent: bool, outcome: &str, bytes: u64) {
        let metric = match (torrent, outcome) {
            (false, "completed") => "http_completed",
            (true, "completed") => "torrent_completed",
            (false, "failed") => "http_failed",
            (true, "failed") => "torrent_failed",
            (false, "cancelled") => "http_cancelled",
            (true, "cancelled") => "torrent_cancelled",
            _ => return,
        };
        let mut state = self.0.lock().unwrap();
        if !state.enabled || state.epoch != sample.epoch || !sample.registered || sample.terminal {
            return;
        }
        sample.terminal = true;
        Self::increment(&mut state, metric);
        if outcome == "completed" {
            Self::increment(
                &mut state,
                if bytes < 10 * 1024 * 1024 {
                    "size_under_10mb"
                } else if bytes < 1024 * 1024 * 1024 {
                    "size_10mb_1gb"
                } else {
                    "size_over_1gb"
                },
            );
        }
    }
    fn take_report(&self) -> Option<(Report, CancellationToken)> {
        let mut state = self.0.lock().unwrap();
        if !state.enabled || state.counters.is_empty() {
            return None;
        }
        Some((
            Report {
                schema: SCHEMA,
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
                counters: std::mem::take(&mut state.counters),
            },
            state.cancel.clone(),
        ))
    }
    pub async fn run(self: std::sync::Arc<Self>) {
        let Ok(client) = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(8))
            .build()
        else {
            return;
        };
        // RAM-only best effort: loss on exit/failure is preferable to a tracking queue.
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(1800));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        timer.tick().await;
        loop {
            timer.tick().await;
            if let Some((report, cancel)) = self.take_report() {
                tokio::select! { biased;
                    _ = cancel.cancelled() => {},
                    _ = client.post(ENDPOINT).json(&report).send() => {},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consent_is_explicit_and_versioned() {
        assert!(!Consent::default().active());
        assert!(!serde_json::from_str::<Consent>("{}").unwrap().active());
        assert!(!Consent::choice(false).active());
        let mut consent = Consent::choice(true);
        assert!(consent.active());
        consent.schema += 1;
        assert!(!consent.active());
        consent.schema = SCHEMA;
        consent.purpose = "new purpose".into();
        assert!(!consent.active());
    }
    #[test]
    fn no_collection_before_consent_no_backfill_or_retry_after_revocation() {
        let analytics = Analytics::new(&Consent::default());
        analytics.feature("pause");
        assert!(analytics.prepare().is_none());
        assert!(analytics.take_report().is_none());
        analytics.configure(&Consent::choice(true));
        assert!(
            analytics.take_report().is_none(),
            "Grant does not backfill launch or history"
        );
        let mut sample = analytics.prepare().unwrap();
        analytics.added(&mut sample, false, false, false, false);
        let (_, cancellation) = analytics.take_report().unwrap();
        analytics.feature("pause");
        analytics.configure(&Consent::choice(false));
        assert!(cancellation.is_cancelled());
        assert!(analytics.take_report().is_none());
        analytics.configure(&Consent::choice(true));
        analytics.outcome(&mut sample, false, "completed", 12);
        assert!(
            analytics.take_report().is_none(),
            "Old transfers cannot rejoin a new consent epoch"
        );
    }
    #[test]
    fn payload_is_bounded_and_outcomes_are_coalesced() {
        let analytics = Analytics::new(&Consent::choice(true));
        let mut sample = analytics.prepare().unwrap();
        analytics.added(&mut sample, true, true, true, true);
        analytics.added(&mut sample, true, true, true, true);
        analytics.outcome(&mut sample, true, "completed", 2 * 1024 * 1024 * 1024);
        analytics.outcome(&mut sample, true, "completed", 42);
        for _ in 0..2000 {
            analytics.feature("pause");
            analytics.feature("https://private.example");
        }
        let (report, _) = analytics.take_report().unwrap();
        assert_eq!(report.counters["torrent_completed"], 1);
        assert_eq!(report.counters["pause"], MAX_COUNT);
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 5);
        assert!(json.to_string().len() < 2048);
        assert!(!json.to_string().contains("private"));
        assert!(analytics.take_report().is_none());
    }
}

/// Per-run consent is separate from usage telemetry. Only fixed coarse diagnostic bands qualify.
pub async fn share_diagnostics(
    counters: BTreeMap<String, u32>,
    version: &str,
    os: &str,
    arch: &str,
    cancel: CancellationToken,
) -> Result<(), String> {
    if !["windows", "linux", "other"].contains(&os)
        || !["x64", "arm64", "other"].contains(&arch)
        || version.split('.').count() != 3
        || version.split('.').any(|part| {
            part.is_empty()
                || part.len() > 3
                || !part.bytes().all(|b| b.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return Err("Invalid diagnostic report metadata.".into());
    }
    if ["cpu", "drive", "memory", "network"].iter().any(|group| {
        counters
            .keys()
            .filter(|key| key.starts_with(&format!("diagnostics_{group}_")))
            .count()
            > 1
    }) {
        return Err("Only one band per diagnostic group may be shared.".into());
    }
    if counters.is_empty()
        || counters.len() > 4
        || counters.iter().any(|(key, count)| {
            *count != 1
                || !["cpu", "drive", "memory", "network"].iter().any(|group| {
                    ["f", "d", "c", "b", "a", "s", "ss"]
                        .iter()
                        .any(|grade| key == &format!("diagnostics_{group}_{grade}"))
                })
        })
    {
        return Err("Invalid coarse diagnostic summary.".into());
    }
    let report = serde_json::json!({"schema":1,"purpose":"diagnostics-summary-v1","version":version,"os":os,"arch":arch,"counters":counters});
    let client = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|_| "Could not initialize summary sharing.")?;
    tokio::select! { biased;
        _=cancel.cancelled()=>Err("Sharing cancelled. The report stays local.".into()),
        result=client.post(ENDPOINT).json(&report).send()=>match result {Ok(response) if response.status().is_success()=>Ok(()),_=>Err("Summary was not confirmed. No retry is queued; the report stays available locally.".into())},
    }
}

#[cfg(test)]
mod diagnostic_sharing_tests {
    use super::*;
    #[tokio::test]
    async fn multiple_bands_and_sensitive_metadata_are_rejected_before_transport() {
        let duplicate = BTreeMap::from([
            ("diagnostics_cpu_f".into(), 1),
            ("diagnostics_cpu_s".into(), 1),
        ]);
        let error = share_diagnostics(
            duplicate,
            "0.6.1",
            "windows",
            "x64",
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("one band"));
        let coarse = BTreeMap::from([("diagnostics_cpu_b".into(), 1)]);
        assert!(share_diagnostics(
            coarse,
            "https://private.example",
            "windows",
            "x64",
            CancellationToken::new()
        )
        .await
        .is_err());
    }
}
