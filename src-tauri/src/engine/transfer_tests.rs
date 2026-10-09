use super::*;

// HTTP, cancellation, retry, resume and shared speed-limit checks run against the real
// application in scripts/test-downloads.mjs, including the durable checkpoint path.
#[test]
fn local_history_keeps_integrity_fields_and_defaults_new_progress_options() {
    let record: DownloadRecord = serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(), "url": "https://example.com/file.bin",
        "fileName": "file.bin", "destination": "C:\\Downloads\\file.bin",
        "status": "paused", "totalBytes": 100, "downloadedBytes": 50,
        "speedBps": 0, "etaSeconds": null, "connections": 4, "error": null,
        "createdAt": Utc::now(), "finishedAt": null,
        "expectedSha256": "trusted digest", "requiresSession": true,
        "requestContext": { "cookie": "session=secret" }
    }))
    .unwrap();
    assert_eq!(record.expected_sha256.as_deref(), Some("trusted digest"));
    assert!(record.requires_session);
    assert_eq!(record.speed_limit_bps, 0);
    assert_eq!(record.merged_bytes, 0);
    assert_eq!(record.resume_supported, None);
    assert!(!record.progress_requested);
    assert!(!record.completion_options.turn_off_computer);
    assert!(!serde_json::to_string(&record).unwrap().contains("secret"));
}
