//! Optional first-party summaries. No caller identifiers or transfer details leave this module.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Mutex};
use tokio_util::sync::CancellationToken;

pub const SCHEMA: u32 = 1;
pub const PURPOSE: &str = "product-improvement-v1";
const ENDPOINT: &str = "https://fetchrail.rrmtools.uk/api/analytics";
const MAX_COUNT: u32 = 1000;
pub(crate) fn valid_version(version: &str) -> bool {
    version.split('.').count() == 3
        && version.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 3
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
}
fn report_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(8))
        .build()
}
async fn post_once(
    client: &reqwest::Client,
    endpoint: &str,
    body: Vec<u8>,
    cancel: CancellationToken,
) -> bool {
    tokio::select! { biased;
        _ = cancel.cancelled() => false,
        result = client.post(endpoint).header(reqwest::header::CONTENT_TYPE, "application/json").body(body).send() => result.is_ok_and(|response| response.status().is_success()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchAck {
    schema: u32,
    batch_id: String,
    accepted: bool,
    duplicate: bool,
}
#[derive(Debug, PartialEq, Eq)]
enum DeliveryResult {
    Acknowledged,
    Retry,
    Rejected,
}
async fn post_acknowledged(
    client: &reqwest::Client,
    endpoint: &str,
    body: Vec<u8>,
    batch_id: &str,
    cancel: CancellationToken,
) -> DeliveryResult {
    tokio::select! { biased;
        _ = cancel.cancelled() => DeliveryResult::Retry,
        result = async {
            let Ok(mut response) = client.post(endpoint).header(reqwest::header::CONTENT_TYPE, "application/json").body(body).send().await else { return DeliveryResult::Retry; };
            if response.status().is_client_error() && !matches!(response.status().as_u16(), 408 | 429) { return DeliveryResult::Rejected; }
            if response.status().as_u16() != 200 { return DeliveryResult::Retry; }
            let mut bytes = Vec::new();
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) if bytes.len() + chunk.len() <= 512 => bytes.extend_from_slice(&chunk),
                    Ok(None) => break,
                    _ => return DeliveryResult::Retry,
                }
            }
            match serde_json::from_slice::<BatchAck>(&bytes) {
                Ok(ack) if ack.schema == crate::quality::SCHEMA && ack.batch_id == batch_id && ack.accepted => {
                    let _ = ack.duplicate;
                    DeliveryResult::Acknowledged
                }
                _ => DeliveryResult::Retry,
            }
        } => result,
    }
}

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
pub struct Analytics {
    state: Mutex<State>,
    pub quality: crate::quality::Quality,
}
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
        let analytics = Self {
            state: Mutex::new(State::default()),
            quality: Default::default(),
        };
        analytics.configure(consent);
        // An app launch is counted only when consent already exists. Never unique users.
        analytics.feature("launch");
        analytics
    }
    pub fn configure(&self, consent: &Consent) {
        let mut state = self.state.lock().unwrap();
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
        let mut state = self.state.lock().unwrap();
        if state.enabled {
            Self::increment(&mut state, metric);
        }
    }
    pub fn prepare(&self) -> Option<Sample> {
        let state = self.state.lock().unwrap();
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
        let mut state = self.state.lock().unwrap();
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
        let mut state = self.state.lock().unwrap();
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
        let mut state = self.state.lock().unwrap();
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
        // Isolated QA grants must not turn routine test workloads into production traffic.
        #[cfg(feature = "test-tools")]
        if std::env::var("FETCHRAIL_TEST_ANALYTICS_CANARY").as_deref() != Ok("1")
            || !matches!(crate::platform::test_root(), Ok(Some(_)))
        {
            return;
        }
        let Ok(client) = report_client() else {
            return;
        };
        // Legacy consent never authorizes the separately renewed v3 scope/outbox.
        let legacy = async {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(1800));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            timer.tick().await;
            loop {
                timer.tick().await;
                if let Some((report, cancel)) = self.take_report() {
                    if let Ok(body) = serde_json::to_vec(&report) {
                        let _ = post_once(&client, ENDPOINT, body, cancel).await;
                    }
                }
            }
        };
        let quality = async {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(30));
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                timer.tick().await;
                if !crate::quality::REMOTE_RELEASE_APPROVED {
                    continue;
                }
                if self.quality.checkpoint_delivery().is_err() {
                    continue;
                }
                if let Some((report, cancel)) = self.quality.next_delivery() {
                    let id = report.batch_id().to_owned();
                    if let Ok(body) = serde_json::to_vec(&report) {
                        match post_acknowledged(&client, ENDPOINT, body, &id, cancel).await {
                            DeliveryResult::Acknowledged => {
                                self.quality.finish_delivery(&id, true, false)
                            }
                            DeliveryResult::Rejected => {
                                self.quality.finish_delivery(&id, false, true)
                            }
                            DeliveryResult::Retry => (),
                        }
                    }
                }
            }
        };
        tokio::join!(legacy, quality);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn acknowledged_delivery_requires_exact_bounded_ack_and_retries_transient_failures() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let id = "0123456789abcdef0123456789abcdef";
        let client = report_client().unwrap();
        let ack = serde_json::json!({"schema":3,"batchId":id,"accepted":true,"duplicate":false})
            .to_string();
        let duplicate =
            serde_json::json!({"schema":3,"batchId":id,"accepted":true,"duplicate":true})
                .to_string();
        let wrong = serde_json::json!({"schema":3,"batchId":"00000000000000000000000000000000","accepted":true,"duplicate":false}).to_string();
        for (status, body, expected) in [
            (200, ack, DeliveryResult::Acknowledged),
            (200, duplicate, DeliveryResult::Acknowledged),
            (200, wrong, DeliveryResult::Retry),
            (
                200,
                "malformed acknowledgement".into(),
                DeliveryResult::Retry,
            ),
            (200, "x".repeat(513), DeliveryResult::Retry),
            (204, String::new(), DeliveryResult::Retry),
            (302, String::new(), DeliveryResult::Retry),
            (408, String::new(), DeliveryResult::Retry),
            (429, String::new(), DeliveryResult::Retry),
            (503, String::new(), DeliveryResult::Retry),
            (400, String::new(), DeliveryResult::Rejected),
            (409, String::new(), DeliveryResult::Rejected),
            (413, String::new(), DeliveryResult::Rejected),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/api/analytics", listener.local_addr().unwrap());
            let server = async {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                // Consume the complete fixture POST before closing the socket. On
                // Windows, closing with unread body bytes can reset the connection
                // and discard a valid ACK independently of the client behavior.
                while !request
                    .windows(4)
                    .position(|part| part == b"\r\n\r\n")
                    .is_some_and(|end| request.len() >= end + 4 + 2)
                {
                    let mut bytes = [0u8; 1024];
                    let read = stream.read(&mut bytes).await.unwrap();
                    assert!(read > 0);
                    request.extend_from_slice(&bytes[..read]);
                    assert!(request.len() < 4096);
                }
                let headers = String::from_utf8_lossy(&request)
                    .split("\r\n\r\n")
                    .next()
                    .unwrap()
                    .to_ascii_lowercase();
                for forbidden in ["cookie:", "authorization:", "referer:", "origin:"] {
                    assert!(!headers.contains(forbidden));
                }
                let response = format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).await.unwrap();
            };
            let (actual, _) = tokio::join!(
                post_acknowledged(
                    &client,
                    &endpoint,
                    b"{}".to_vec(),
                    id,
                    CancellationToken::new()
                ),
                server
            );
            assert_eq!(
                actual, expected,
                "HTTP {status} must not acknowledge an unverified batch"
            );
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/analytics", listener.local_addr().unwrap());
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            post_acknowledged(&client, &endpoint, b"{}".to_vec(), id, cancel).await,
            DeliveryResult::Retry
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn withdrawal_cancels_waiting_for_an_ack_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/analytics", listener.local_addr().unwrap());
        let client = report_client().unwrap();
        let quality = crate::quality::Quality::default();
        quality.configure(&crate::quality::Consent::choice(true));
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
        let (report, cancel) = quality.take_report().unwrap();
        let server = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0u8; 1024];
            assert!(stream.read(&mut bytes).await.unwrap() > 0);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                .await
                .unwrap();
            quality.configure(&crate::quality::Consent::choice(false));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        let (actual, _) = tokio::join!(
            post_acknowledged(
                &client,
                &endpoint,
                b"{}".to_vec(),
                report.batch_id(),
                cancel
            ),
            server
        );
        assert_eq!(actual, DeliveryResult::Retry);
        assert!(quality.preview().is_none());
    }
    #[cfg(feature = "test-tools")]
    #[tokio::test]
    async fn test_build_keeps_granted_batches_local_without_an_isolated_canary() {
        assert!(crate::platform::test_root().unwrap().is_none());
        let analytics = std::sync::Arc::new(Analytics::new(&Consent::choice(true)));
        analytics
            .quality
            .configure(&crate::quality::Consent::choice(true));
        let mut sample = analytics.quality.prepare().unwrap();
        analytics.quality.added(&mut sample, false);
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            analytics.clone().run(),
        )
        .await
        .expect("A normal test build must exit before constructing a sender or starting its timer");
        assert!(analytics.quality.preview().is_some());
        assert!(analytics.take_report().is_some());
    }
    #[tokio::test]
    async fn actual_transport_has_no_source_credentials_and_cancelled_batches_open_no_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/analytics", listener.local_addr().unwrap());
        let client = report_client().unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(!post_once(&client, &endpoint, b"{}".to_vec(), cancelled).await);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
        let analytics = Analytics::new(&Consent::default());
        assert!(analytics.take_report().is_none() && analytics.quality.take_report().is_none());
        analytics
            .quality
            .configure(&crate::quality::Consent::choice(true));
        let mut sample = analytics.quality.prepare().unwrap();
        analytics.quality.added(&mut sample, false);
        let (report, cancel) = analytics.quality.take_report().unwrap();
        let body = serde_json::to_vec(&report).unwrap();
        let receive = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buffer[..n]);
                assert!(bytes.len() < 8192);
                if bytes
                    .windows(4)
                    .position(|v| v == b"\r\n\r\n")
                    .is_some_and(|end| bytes.len() >= end + 4 + body.len())
                {
                    break;
                }
            }
            let request = String::from_utf8(bytes).unwrap();
            let headers = request
                .split("\r\n\r\n")
                .next()
                .unwrap()
                .to_ascii_lowercase();
            for sensitive in ["cookie:", "authorization:", "referer:", "origin:"] {
                assert!(!headers.contains(sensitive));
            }
            assert!(request.contains("product-quality-v3"));
            assert!(!request.contains("decidedAt"));
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        };
        let (sent, _) = tokio::join!(post_once(&client, &endpoint, body.clone(), cancel), receive);
        assert!(sent);
        assert!(analytics.take_report().is_none(), "v2 cannot enable v1");
        // An already received request cannot be recalled; withdrawal still aborts the waiting client.
        let mut sample = analytics.quality.prepare().unwrap();
        analytics.quality.added(&mut sample, false);
        let (report, cancel) = analytics.quality.take_report().unwrap();
        let receive = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            assert!(stream.read(&mut buffer).await.unwrap() > 0);
            analytics
                .quality
                .configure(&crate::quality::Consent::choice(false));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };
        let (sent, _) = tokio::join!(
            post_once(
                &client,
                &endpoint,
                serde_json::to_vec(&report).unwrap(),
                cancel
            ),
            receive
        );
        assert!(!sent);
        assert!(analytics.quality.take_report().is_none());
    }
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
        || !valid_version(version)
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
