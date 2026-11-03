//! Hash-gated multi-source transfer qualification and redacted diagnostics.
//! Strong ETags are scoped to *one* origin; identical-looking ETags on two
//! different sites are never treated as proof of equivalent content.

use super::*;

#[derive(Clone)]
pub(super) struct QualifiedMirror {
    pub url: Url,
    pub probe: ProbeResult,
    pub ordinal: usize,
    pub attempts: Arc<AtomicU64>,
    pub failures: Arc<AtomicU64>,
}

impl QualifiedMirror {
    fn new(url: Url, probe: ProbeResult, ordinal: usize) -> Self {
        Self {
            url,
            probe,
            ordinal,
            attempts: Arc::new(AtomicU64::new(0)),
            failures: Arc::new(AtomicU64::new(0)),
        }
    }

    #[cfg(test)]
    pub(super) fn fixture(url: Url, probe: ProbeResult, ordinal: usize) -> Self {
        Self::new(url, probe, ordinal)
    }
}

pub(super) fn matches_primary(primary: &ProbeResult, candidate: &ProbeResult) -> bool {
    // The primary must be a valid *ranged*, strongly validated representation.
    // A trusted SHA-256 supplied by the user/Metalink is checked separately.
    let Some(size) = primary.total_bytes.filter(|size| *size > 0) else {
        return false;
    };
    primary.accepts_ranges
        && primary.validator.is_some()
        && candidate.accepts_ranges
        && candidate.validator.is_some()
        && candidate.total_bytes == Some(size)
        && candidate.final_url.is_some()
}

pub(super) fn redacted_report(
    candidates: &[QualifiedMirror],
    rejected: &[(usize, &'static str)],
) -> serde_json::Value {
    let mut entries = candidates
        .iter()
        .map(|candidate| {
            json!({
                "source": candidate.ordinal,
                "ready": true,
                "attempts": candidate.attempts.load(Ordering::Relaxed),
                "failures": candidate.failures.load(Ordering::Relaxed),
            })
        })
        .collect::<Vec<_>>();
    for (ordinal, reason) in rejected {
        entries.push(json!({ "source": ordinal, "ready": false, "reason": reason,
            "attempts": 0, "failures": 0 }));
    }
    entries.sort_by_key(|entry| entry["source"].as_u64().unwrap_or(u64::MAX));
    json!(entries)
}

impl DownloadManager {
    pub(super) async fn qualify_mirror_sources(
        &self,
        task: &Arc<DownloadTask>,
        primary_url: &Url,
        primary_probe: &ProbeResult,
        cancel: &CancellationToken,
    ) -> EngineResult<()> {
        task.qualified_mirrors.lock().unwrap().clear();
        let urls = task.mirror_urls.lock().unwrap().clone();
        if urls.is_empty() {
            return Ok(());
        }
        let trustworthy = task.record.read().await.expected_sha256.is_some()
            && task.request_context.is_none()
            && task.browser_verification.lock().unwrap().is_none()
            && matches!(primary_url.scheme(), "http" | "https")
            && matches_primary(primary_probe, primary_probe);
        if !trustworthy {
            // No implicit mixing if primary has no strong range validator or
            // if this is a browser-authenticated transfer.
            let mut record = task.record.write().await;
            let ignored = (2..=urls.len() + 1).map(|ordinal| (ordinal, "primary_not_qualified"));
            record.extra.insert(
                "mirrorStatus".into(),
                redacted_report(&[], &ignored.collect::<Vec<_>>()),
            );
            return Ok(());
        }

        let mut qualified = vec![QualifiedMirror::new(
            primary_url.clone(),
            primary_probe.clone(),
            1,
        )];
        let mut rejected = Vec::new();
        // Each metadata request is independently admitted, timed and cancelled.
        // Process at most seven optional sources, without exposing signed URLs
        // in user-facing errors or progress events.
        let probes = urls.iter().enumerate().map(|(index, source)| async move {
            let result = tokio::select! {
                _ = cancel.cancelled() => return Err(EngineError::Cancelled),
                result = tokio::time::timeout(Duration::from_secs(12), self.probe(source, None, None)) => result,
            };
            Ok((index + 2, source.clone(), result))
        });
        for result in join_all(probes).await {
            task.check_stopped(cancel)?;
            let (ordinal, source, probe) = result?;
            match probe {
                Ok(Ok(probe)) if matches_primary(primary_probe, &probe) => {
                    qualified.push(QualifiedMirror::new(source, probe, ordinal));
                }
                Ok(Ok(_)) => rejected.push((ordinal, "different_size_or_range_validator")),
                Ok(Err(_)) => rejected.push((ordinal, "probe_failed")),
                Err(_) => rejected.push((ordinal, "probe_timeout")),
            }
        }
        let report = redacted_report(&qualified, &rejected);
        // If all optional sources fail, keep the original single-origin path.
        // Even a single eligible primary records accurate attempt statistics
        // when the secondary probes failed.
        *task.qualified_mirrors.lock().unwrap() = qualified;
        let snapshot = {
            let mut record = task.record.write().await;
            record.extra.insert("mirrorStatus".into(), report);
            record.clone()
        };
        self.emit_record(&snapshot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unproven_metadata_even_when_etag_text_matches() {
        let probe = ProbeResult {
            total_bytes: Some(512),
            accepts_ranges: true,
            validator: Some(StrongEtag("\"v1\"".into())),
            final_url: Some(Url::parse("https://a.example.com/file").unwrap()),
            suggested_file_name: None,
        };
        let mut changed = probe.clone();
        changed.total_bytes = Some(511);
        assert!(!matches_primary(&probe, &changed));
        changed = probe.clone();
        changed.validator = None;
        assert!(!matches_primary(&probe, &changed));
        changed = probe.clone();
        changed.accepts_ranges = false;
        assert!(!matches_primary(&probe, &changed));
        assert!(matches_primary(&probe, &probe));
    }

    #[test]
    fn diagnostics_never_serialize_signed_mirror_urls() {
        let source = Url::parse("https://cdn.example.com/file?token=very-secret").unwrap();
        let probe = ProbeResult {
            total_bytes: Some(512),
            accepts_ranges: true,
            validator: Some(StrongEtag("\"ok\"".into())),
            final_url: Some(source.clone()),
            suggested_file_name: None,
        };
        let report = redacted_report(
            &[QualifiedMirror::new(source, probe, 2)],
            &[(3, "probe_failed")],
        )
        .to_string();
        assert!(!report.contains("token"));
        assert!(!report.contains("cdn.example.com"));
        assert!(report.contains("probe_failed"));
    }
}
