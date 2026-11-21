//! Private, short-lived batch outbox. No backups and no persistent caller identity.
use super::{Quality, Report, PURPOSE, SCHEMA};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
const FILE: &str = "quality-outbox.json";
const FILE_PURPOSE: &str = "quality-delivery-v3";
const MAX_BATCHES: usize = 64;
const MAX_BYTES: usize = 256 * 1024;
const TTL: i64 = 7 * 86400;
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Counts {
    acknowledged: u64,
    attempts: u64,
    expired: u64,
    overflow_dropped: u64,
    rejected: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batch {
    created_at: i64,
    next_attempt_at: i64,
    attempts: u32,
    report: Report,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    schema: u32,
    purpose: String,
    grant: String,
    batches: Vec<Batch>,
    counts: Counts,
}
#[derive(Clone, Default)]
pub(super) struct Delivery {
    root: Option<PathBuf>,
    grant: String,
    batches: Vec<Batch>,
    counts: Counts,
    error: Option<String>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryStatus {
    pub configured: bool,
    pub collecting: bool,
    pub pending_batches: usize,
    pub pending_bytes: usize,
    pub oldest_age_seconds: Option<u64>,
    pub next_attempt_seconds: Option<u64>,
    pub acknowledged_batches: u64,
    pub attempted_batches: u64,
    pub expired_batches: u64,
    pub overflow_dropped_batches: u64,
    pub rejected_batches: u64,
    pub storage_error: Option<String>,
    pub last_delivery: &'static str,
}
fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
fn read(root: &Path) -> Result<Option<Stored>, String> {
    read_path(&root.join(FILE))
}
fn read_path(path: &Path) -> Result<Option<Stored>, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta)
            if meta.is_file()
                && !meta.file_type().is_symlink()
                && meta.len() <= (MAX_BYTES * 2) as u64 =>
        {
            ()
        }
        _ => {
            return Err("Unrecognized reliability outbox was preserved; delivery is paused.".into())
        }
    }
    let stored: Stored = crate::state_store::read(path)
        .map_err(|_| "Damaged reliability outbox was preserved; delivery is paused.")?;
    if stored.schema != 1
        || stored.purpose != FILE_PURPOSE
        || stored.grant.len() > 128
        || stored.batches.len() > MAX_BATCHES
        || stored.batches.iter().any(|batch| !batch.report.valid())
    {
        return Err("Unrecognized reliability outbox was preserved; delivery is paused.".into());
    }
    if serde_json::to_vec(&stored)
        .map_err(|_| "Cannot inspect reliability outbox.")?
        .len()
        > MAX_BYTES
    {
        return Err("Oversized reliability outbox was preserved; delivery is paused.".into());
    }
    Ok(Some(stored))
}
// private_atomic removes staging files on ordinary errors, but a hard crash can
// leave one. Clear only this purpose's validated files, never arbitrary *.tmp.
fn clear_temporary(root: &Path) -> Result<(), String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err("Cannot inspect reliability staging files; delivery is paused.".into())
        }
    };
    let mut failure = None;
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err(
                "Reliability staging inspection exceeded its bound; delivery is paused.".into(),
            );
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                failure = Some(
                    "Cannot inspect reliability staging files; delivery is paused.".to_string(),
                );
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(nonce) = name
            .strip_prefix("quality-outbox.")
            .and_then(|name| name.strip_suffix(".tmp"))
        else {
            continue;
        };
        if !uuid::Uuid::parse_str(nonce).is_ok_and(|id| id.to_string() == nonce) {
            continue;
        }
        match read_path(&entry.path()) {
            Ok(Some(_)) => if let Err(error) = std::fs::remove_file(entry.path()) {
                if error.kind() != std::io::ErrorKind::NotFound { failure = Some("Cannot delete an owned reliability staging file; delivery is paused.".into()); }
            },
            Ok(None) => (),
            Err(_) => failure = Some("Unrecognized reliability staging file was preserved; delivery is paused. Inspect the app data before retrying.".into()),
        }
    }
    failure.map_or(Ok(()), Err)
}
impl Delivery {
    fn stored(&self) -> Stored {
        Stored {
            schema: 1,
            purpose: FILE_PURPOSE.into(),
            grant: self.grant.clone(),
            batches: self.batches.clone(),
            counts: self.counts.clone(),
        }
    }
    fn persist(&self) -> Result<(), String> {
        let root = self
            .root
            .as_deref()
            .ok_or("Reliability outbox storage is unavailable.")?;
        read(root)?; // Preserve foreign/damaged files rather than overwriting them.
        clear_temporary(root)?;
        let bytes =
            serde_json::to_vec(&self.stored()).map_err(|_| "Cannot encode reliability outbox.")?;
        if bytes.len() > MAX_BYTES {
            return Err("Reliability outbox exceeds its bound.".into());
        }
        let protected = crate::state_store::encode(&bytes)
            .map_err(|_| "Cannot protect reliability outbox; delivery is paused.")?;
        crate::platform::write_private_atomic(&root.join(FILE), &protected).map_err(|_| {
            "Cannot save reliability outbox; pending evidence is retained in memory.".into()
        })
    }
    fn clear(&self) -> Result<(), String> {
        if let Some(root) = &self.root {
            let main = read(root).and_then(|stored| if stored.is_some() {
                match std::fs::remove_file(root.join(FILE)) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(_) => Err("Reliability collection is off, but the saved outbox could not be deleted. Retry before restarting.".into()),
                }
            } else { Ok(()) });
            // Attempt both deletions even if one owned/foreign file cannot be cleared.
            let staged = clear_temporary(root);
            main.and(staged)?;
        }
        Ok(())
    }
    pub(super) fn configure(&mut self, grant: &str) {
        if self.grant != grant || grant.is_empty() {
            self.batches.clear();
            self.counts = Counts::default();
            self.error = self.clear().err();
            self.grant = grant.into();
        }
    }
    fn load(&mut self, grant: &str, timestamp: i64) -> Result<(), String> {
        if grant.is_empty() {
            self.grant.clear();
            self.batches.clear();
            self.counts = Counts::default();
            self.clear()?;
            self.error = None;
            return Ok(());
        }
        clear_temporary(
            self.root
                .as_deref()
                .ok_or("Reliability outbox storage is unavailable.")?,
        )?;
        let stored = read(
            self.root
                .as_deref()
                .ok_or("Reliability outbox storage is unavailable.")?,
        )?;
        self.grant = grant.into();
        if let Some(stored) = stored {
            if grant.is_empty() || stored.grant != grant {
                self.clear()?;
                self.batches.clear();
                self.counts = Counts::default();
            } else {
                self.batches = stored.batches;
                self.counts = stored.counts;
                if self.prune(timestamp) {
                    self.persist()?;
                }
            }
        }
        self.error = None;
        Ok(())
    }
    fn prune(&mut self, timestamp: i64) -> bool {
        let before = self.batches.len();
        self.batches.retain(|batch| {
            batch.created_at <= timestamp && batch.created_at > timestamp.saturating_sub(TTL)
        });
        self.counts.expired = self
            .counts
            .expired
            .saturating_add((before - self.batches.len()) as u64);
        before != self.batches.len()
    }
    fn fit(&mut self) -> Result<(), String> {
        while self.batches.len() > MAX_BATCHES
            || serde_json::to_vec(&self.stored())
                .map_err(|_| "Cannot encode reliability outbox.")?
                .len()
                > MAX_BYTES
        {
            if self.batches.is_empty() {
                return Err("Reliability outbox metadata exceeds its bound.".into());
            }
            self.batches.remove(0);
            self.counts.overflow_dropped = self.counts.overflow_dropped.saturating_add(1);
        }
        Ok(())
    }
}
impl Quality {
    /// Called once with the already-private application data directory. Uses no backup generation.
    pub fn set_storage(&self, root: PathBuf) -> Result<(), String> {
        let s = self.0.lock().unwrap();
        let mut delivery = self.1.lock().unwrap();
        delivery.root = Some(root);
        let result = delivery.load(&s.grant, now());
        if let Err(error) = &result {
            delivery.error = Some(error.clone());
        }
        result
    }
    /// Take a bounded durable checkpoint, only after consent. A process exit before this
    /// checkpoint can still lose volatile counters; never advertise exactly-once collection.
    pub fn checkpoint_delivery(&self) -> Result<(), String> {
        self.checkpoint_at(now())
    }
    fn checkpoint_at(&self, timestamp: i64) -> Result<(), String> {
        let mut s = self.0.lock().unwrap();
        let mut delivery = self.1.lock().unwrap();
        if !s.enabled || s.grant.is_empty() {
            return Ok(());
        }
        if delivery.root.is_none() {
            return Err("Reliability outbox storage is unavailable.".into());
        }
        let mut candidate = delivery.clone();
        let pruned = candidate.prune(timestamp);
        if s.counters.is_empty() && !pruned && delivery.error.is_none() {
            return Ok(());
        }
        if !s.counters.is_empty() {
            let report = super::report(s.counters.clone(), s.histograms.clone());
            if !report.valid() {
                let error = "Reliability summary exceeded its finite schema; delivery is paused."
                    .to_string();
                delivery.error = Some(error.clone());
                return Err(error);
            }
            candidate.batches.push(Batch {
                created_at: timestamp,
                next_attempt_at: timestamp,
                attempts: 0,
                report,
            });
            candidate.fit()?;
        }
        match candidate.persist() {
            Ok(()) => {
                candidate.error = None;
                *delivery = candidate;
                s.counters.clear();
                s.histograms.clear();
                Ok(())
            }
            Err(error) => {
                delivery.error = Some(error.clone());
                Err(error)
            }
        }
    }
    pub(crate) fn next_delivery(&self) -> Option<(Report, CancellationToken)> {
        self.next_at(now())
    }
    fn next_at(&self, timestamp: i64) -> Option<(Report, CancellationToken)> {
        let s = self.0.lock().unwrap();
        let mut delivery = self.1.lock().unwrap();
        if !s.enabled || s.grant != delivery.grant || delivery.error.is_some() {
            return None;
        }
        let mut candidate = delivery.clone();
        candidate.prune(timestamp);
        let batch = candidate
            .batches
            .iter_mut()
            .find(|batch| batch.next_attempt_at <= timestamp)?;
        batch.attempts = batch.attempts.saturating_add(1);
        // One explicit application attempt per tick; exponential retry is capped at 30 min.
        let delay =
            (60i64.saturating_mul(1i64 << batch.attempts.saturating_sub(1).min(5))).min(1800);
        batch.next_attempt_at = timestamp.saturating_add(delay);
        let report = batch.report.clone();
        candidate.counts.attempts = candidate.counts.attempts.saturating_add(1);
        match candidate.persist() {
            Ok(()) => {
                *delivery = candidate;
                Some((report, s.cancel.clone()))
            }
            Err(error) => {
                delivery.error = Some(error);
                None
            }
        }
    }
    pub(crate) fn finish_delivery(&self, batch_id: &str, accepted: bool, permanent: bool) {
        let s = self.0.lock().unwrap();
        let mut delivery = self.1.lock().unwrap();
        if !s.enabled || s.grant != delivery.grant {
            return;
        }
        if !accepted && !permanent {
            return;
        } // A pending immutable batch remains retryable.
        if !delivery
            .batches
            .iter()
            .any(|batch| batch.report.batch_id == batch_id)
        {
            return;
        }
        let mut candidate = delivery.clone();
        candidate
            .batches
            .retain(|batch| batch.report.batch_id != batch_id);
        if accepted {
            candidate.counts.acknowledged = candidate.counts.acknowledged.saturating_add(1);
        } else {
            candidate.counts.rejected = candidate.counts.rejected.saturating_add(1);
        }
        match candidate.persist() {
            Ok(()) => {
                candidate.error = None;
                *delivery = candidate;
            }
            // Original durable batch may remain after an ACK, so retry with the same nonce.
            Err(error) => delivery.error = Some(error),
        }
    }
    pub fn delivery_status(&self) -> DeliveryStatus {
        let s = self.0.lock().unwrap();
        let delivery = self.1.lock().unwrap();
        let timestamp = now();
        DeliveryStatus {
            configured: delivery.root.is_some(),
            collecting: s.enabled,
            pending_batches: delivery.batches.len(),
            pending_bytes: delivery
                .batches
                .iter()
                .map(|batch| serde_json::to_vec(&batch.report).map_or(0, |bytes| bytes.len()))
                .sum(),
            oldest_age_seconds: delivery
                .batches
                .iter()
                .map(|batch| timestamp.saturating_sub(batch.created_at).max(0) as u64)
                .max(),
            next_attempt_seconds: delivery
                .batches
                .iter()
                .map(|batch| batch.next_attempt_at.saturating_sub(timestamp).max(0) as u64)
                .min(),
            acknowledged_batches: delivery.counts.acknowledged,
            attempted_batches: delivery.counts.attempts,
            expired_batches: delivery.counts.expired,
            overflow_dropped_batches: delivery.counts.overflow_dropped,
            rejected_batches: delivery.counts.rejected,
            storage_error: delivery.error.clone(),
            last_delivery: if delivery.error.is_some() {
                "storage_error"
            } else if !delivery.batches.is_empty() {
                "pending_or_retrying"
            } else if delivery.counts.acknowledged > 0 {
                "acknowledged"
            } else {
                "none"
            },
        }
    }
}
impl Report {
    fn valid(&self) -> bool {
        const COUNTERS: &[&str] = &[
            "http_accepted",
            "torrent_accepted",
            "http_verified_durable",
            "torrent_selected_ready",
            "http_failed",
            "torrent_failed",
            "http_cancelled",
            "torrent_cancelled",
            "http1_responses",
            "http2_responses",
            "other_protocol_responses",
            "torrent_hash_failures",
            "torrent_file_errors",
            "torrent_engine_errors",
            "torrent_metadata_failures",
            "torrent_checkpoint_errors",
            "receive_stalls",
            "size_unknown",
            "size_small",
            "size_medium",
            "size_large",
            "e2e_unobserved",
            "clipped",
            "throughput_unobserved",
            "delivery_cap_observed",
        ];
        const SIZES: &[&str] = &[
            "unknown", "under10m", "10m1g", "1g4g", "4g16g", "16g64g", "over64g",
        ];
        self.schema == SCHEMA
            && self.purpose == PURPOSE
            && crate::analytics::valid_version(&self.version)
            && ["windows", "linux", "other"].contains(&self.os.as_str())
            && ["x64", "arm64", "other"].contains(&self.arch.as_str())
            && ["production", "controlled"].contains(&self.provenance.as_str())
            && self.batch_id.len() == 32
            && self
                .batch_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && !self.counters.is_empty()
            && self.counters.iter().all(|(key, count)| {
                *count > 0
                    && *count <= 1000
                    && (key != "clipped" || *count == 1)
                    && (COUNTERS.contains(&key.as_str())
                        || crate::health::CAUSES
                            .iter()
                            .any(|cause| super::cause_key(*cause) == key))
            })
            && self.histograms.iter().all(|(key, bins)| {
                let allowed = key == "stall_total_ms_v1"
                    || crate::health::PHASES
                        .iter()
                        .any(|phase| super::stage_key(*phase) == Some(key.as_str()))
                    || SIZES.iter().any(|size| {
                        key == &format!("http_e2e_{size}_ms_v2")
                            || key == &format!("http_delivery_{size}_mbps_v1")
                    });
                allowed
                    && bins.len() == super::histogram_bounds(key).len() + 1
                    && bins.iter().any(|n| *n > 0)
                    && bins.iter().all(|n| *n <= 1000)
            })
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 8192)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, super::super::Consent, Quality) {
        let root =
            std::env::temp_dir().join(format!("fetchrail-quality-outbox-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let quality = Quality::default();
        let consent = super::super::Consent::choice(true);
        quality.configure(&consent);
        quality.set_storage(root.clone()).unwrap();
        (root, consent, quality)
    }
    fn add(quality: &Quality) {
        let mut sample = quality.prepare().unwrap();
        quality.added(&mut sample, false);
    }
    #[test]
    fn durable_retry_survives_restart_with_same_nonce_and_clears_on_ack() {
        let (root, consent, quality) = fixture();
        add(&quality);
        quality.checkpoint_at(1_000_000).unwrap();
        let first = quality.next_at(1_000_000).unwrap().0;
        assert!(quality.next_at(1_000_059).is_none());
        let restarted = Quality::default();
        restarted.configure(&consent);
        {
            let s = restarted.0.lock().unwrap();
            let mut d = restarted.1.lock().unwrap();
            d.root = Some(root.clone());
            d.load(&s.grant, 1_000_001).unwrap();
        }
        let second = restarted.next_at(1_000_060).unwrap().0;
        assert_eq!(first.batch_id, second.batch_id);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
        restarted.finish_delivery(&second.batch_id, true, false);
        assert_eq!(restarted.delivery_status().pending_batches, 0);
        assert_eq!(read(&root).unwrap().unwrap().counts.acknowledged, 1);
        assert!(!root.join("quality-outbox.json.backup").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn withdrawal_cancels_inflight_deletes_queue_and_old_grant_cannot_rejoin() {
        let (root, consent, quality) = fixture();
        add(&quality);
        quality.checkpoint_at(1_000_000).unwrap();
        let (_, token) = quality.next_at(1_000_000).unwrap();
        quality.configure(&super::super::Consent::choice(false));
        assert!(token.is_cancelled());
        assert!(!root.join(FILE).exists());
        quality.finish_delivery("irrelevant", true, false);
        quality.configure(&consent);
        assert_eq!(quality.delivery_status().pending_batches, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn expiry_overflow_foreign_file_and_failed_checkpoint_do_not_leak_or_drain() {
        let (root, _, quality) = fixture();
        for i in 0..70 {
            add(&quality);
            quality.checkpoint_at(1_000_000 + i).unwrap();
        }
        assert_eq!(quality.delivery_status().pending_batches, MAX_BATCHES);
        assert_eq!(quality.delivery_status().overflow_dropped_batches, 6);
        quality.checkpoint_at(1_000_100 + TTL).unwrap();
        assert_eq!(quality.delivery_status().pending_batches, 0);
        assert_eq!(
            quality.delivery_status().expired_batches,
            MAX_BATCHES as u64
        );
        std::fs::write(root.join(FILE), b"foreign user data").unwrap();
        add(&quality);
        assert!(quality.checkpoint_at(1_000_200 + TTL).is_err());
        assert_eq!(quality.preview().unwrap().counters["http_accepted"], 1);
        assert!(quality.next_at(1_000_200 + TTL).is_none());
        quality.configure(&super::super::Consent::choice(false));
        assert_eq!(
            std::fs::read(root.join(FILE)).unwrap(),
            b"foreign user data"
        );
        assert!(quality.delivery_status().storage_error.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn a_new_consent_epoch_discards_old_durable_batches() {
        let (root, consent, quality) = fixture();
        add(&quality);
        quality.checkpoint_at(1_000_000).unwrap();
        let mut new_consent = consent;
        new_consent.decided_at.push('Z');
        quality.configure(&new_consent);
        assert!(quality.next_at(1_000_001).is_none());
        assert!(!root.join(FILE).exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn crash_staging_copies_are_removed_but_foreign_or_damaged_files_are_preserved() {
        let (root, _, quality) = fixture();
        add(&quality);
        quality.checkpoint_at(1_000_000).unwrap();
        let orphan = root.join(format!("quality-outbox.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::copy(root.join(FILE), &orphan).unwrap();
        let foreign = root.join("unrelated.tmp");
        std::fs::write(&foreign, b"foreign").unwrap();
        quality.configure(&super::super::Consent::choice(false));
        assert!(!root.join(FILE).exists());
        assert!(!orphan.exists());
        assert!(foreign.exists());
        let damaged = root.join(format!("quality-outbox.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&damaged, b"partial or foreign content").unwrap();
        quality.configure(&super::super::Consent::choice(false));
        assert!(damaged.exists());
        assert!(quality.delivery_status().storage_error.is_some());
        std::fs::remove_file(damaged).unwrap();
        quality.configure(&super::super::Consent::choice(false));
        assert!(quality.delivery_status().storage_error.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }
}
