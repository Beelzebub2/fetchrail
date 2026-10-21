use super::*;

struct PreparedFixture {
    root: PathBuf,
    parts: PathBuf,
    candidate: PathBuf,
    neighbor: PathBuf,
    record: DownloadRecord,
    checkpoint: Vec<u8>,
    manifest: Vec<u8>,
}

impl PreparedFixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fetchrail-owned-cleanup-{}", Uuid::new_v4()));
        let id = Uuid::new_v4();
        let parts = root.join("parts").join(id.to_string());
        std::fs::create_dir_all(&parts).unwrap();
        let destination = root.join("file.txt");
        let record: DownloadRecord = serde_json::from_value(json!({
            "id": id, "url": "http://127.0.0.1/file.txt", "fileName": "file.txt",
            "destination": destination, "status": "failed", "totalBytes": 7,
            "downloadedBytes": 7, "speedBps": 0, "etaSeconds": null,
            "connections": 1, "error": "Attachment approval failed",
            "createdAt": Utc::now(), "finishedAt": Utc::now(),
            "expectedSha256": format!("{:x}", Sha256::digest(b"payload")),
        }))
        .unwrap();
        let manifest = serde_json::to_vec_pretty(&PartManifest {
            total_bytes: Some(7),
            ranges: vec![ByteRange { start: 0, end: 6 }],
            validator: Some("\"fixture-v1\"".into()),
            version: 2,
            final_url_sha256: None,
            accepts_ranges: true,
            shared_staging: true,
        })
        .unwrap();
        std::fs::write(parts.join("transfer.json"), &manifest).unwrap();
        let staging = parts.join("0.part");
        std::fs::write(&staging, b"payload").unwrap();
        let candidate = root.join(format!(
            ".file.txt.{id}.{}.fetchrail-part.txt",
            Uuid::new_v4()
        ));
        // The usual same-volume assembly shares an inode with its durable part.
        std::fs::hard_link(&staging, &candidate).unwrap();
        let identity = open_publication_file(&candidate).unwrap().0;
        let checkpoint = serde_json::to_vec_pretty(&MergeCheckpoint {
            file_name: candidate.file_name().unwrap().to_str().unwrap().into(),
            total: 7,
            fingerprint: format!("{:x}", Sha256::digest(&manifest)),
            expected_sha256: record.expected_sha256.clone(),
            identity: Some(identity),
        })
        .unwrap();
        std::fs::write(parts.join("merge.json"), &checkpoint).unwrap();
        let neighbor = root.join(format!(
            ".file.txt.{}.{}.fetchrail-part.txt",
            Uuid::new_v4(),
            Uuid::new_v4()
        ));
        std::fs::write(&neighbor, b"another job").unwrap();
        Self {
            root,
            parts,
            candidate,
            neighbor,
            record,
            checkpoint,
            manifest,
        }
    }

    fn assert_recoverable(&self, checkpoint: &[u8]) {
        assert_eq!(std::fs::read(&self.candidate).unwrap(), b"payload");
        assert_eq!(
            std::fs::read(self.parts.join("0.part")).unwrap(),
            b"payload"
        );
        assert_eq!(std::fs::read(&self.neighbor).unwrap(), b"another job");
        assert_eq!(
            std::fs::read(self.parts.join("merge.json")).unwrap(),
            checkpoint
        );
        assert_eq!(
            std::fs::read(self.parts.join("transfer.json")).unwrap(),
            self.manifest
        );
    }
}

impl Drop for PreparedFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(windows)]
#[test]
fn prepared_cleanup_deletes_only_owned_link_and_retains_proof_until_caller_commits() {
    let fixture = PreparedFixture::new();
    remove_prepared_output(&fixture.parts, &fixture.record).unwrap();
    assert!(!fixture.candidate.exists());
    assert_eq!(std::fs::read(&fixture.neighbor).unwrap(), b"another job");
    assert_eq!(
        std::fs::read(fixture.parts.join("0.part")).unwrap(),
        b"payload"
    );
    assert_eq!(
        std::fs::read(fixture.parts.join("merge.json")).unwrap(),
        fixture.checkpoint
    );
    // Retry after successful output cleanup but interrupted staging cleanup.
    remove_prepared_output(&fixture.parts, &fixture.record).unwrap();
}

#[test]
fn prepared_cleanup_rejects_mismatched_or_damaged_proof_without_losing_recovery() {
    let fixture = PreparedFixture::new();
    for mismatch in [
        "name",
        "identity",
        "manifest",
        "hash_policy",
        "total",
        "damaged",
        "oversized",
    ] {
        let mut checkpoint: serde_json::Value =
            serde_json::from_slice(&fixture.checkpoint).unwrap();
        match mismatch {
            "name" => {
                checkpoint["file_name"] =
                    json!(fixture.neighbor.file_name().unwrap().to_str().unwrap())
            }
            "identity" => checkpoint["identity"] = json!({ "volume": 0, "file": 0 }),
            "manifest" => checkpoint["fingerprint"] = json!("0".repeat(64)),
            "hash_policy" => checkpoint["expected_sha256"] = json!("0".repeat(64)),
            "total" => checkpoint["total"] = json!(8),
            _ => {}
        }
        let bytes = match mismatch {
            "damaged" => b"{".to_vec(),
            "oversized" => vec![b' '; 64 * 1024 + 1],
            _ => serde_json::to_vec(&checkpoint).unwrap(),
        };
        std::fs::write(fixture.parts.join("merge.json"), &bytes).unwrap();
        assert!(
            remove_prepared_output(&fixture.parts, &fixture.record).is_err(),
            "{mismatch}"
        );
        fixture.assert_recoverable(&bytes);
    }
}

#[test]
fn prepared_cleanup_rejects_foreign_occupant_even_when_its_bytes_are_identical() {
    let fixture = PreparedFixture::new();
    let retained = fixture.root.join("original-identity.txt");
    std::fs::rename(&fixture.candidate, &retained).unwrap();
    std::fs::write(&fixture.candidate, b"payload").unwrap();
    let error = remove_prepared_output(&fixture.parts, &fixture.record).unwrap_err();
    assert!(error.to_string().contains("identity"));
    fixture.assert_recoverable(&fixture.checkpoint);
    assert_eq!(std::fs::read(&retained).unwrap(), b"payload");
}

#[cfg(windows)]
#[test]
fn prepared_cleanup_locked_file_keeps_checkpoint_and_can_be_retried() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = PreparedFixture::new();
    let pin = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&fixture.candidate)
        .unwrap();
    assert!(remove_prepared_output(&fixture.parts, &fixture.record).is_err());
    fixture.assert_recoverable(&fixture.checkpoint);
    drop(pin);
    remove_prepared_output(&fixture.parts, &fixture.record).unwrap();
    assert!(!fixture.candidate.exists());
    assert_eq!(
        std::fs::read(fixture.parts.join("merge.json")).unwrap(),
        fixture.checkpoint
    );
}

#[test]
fn prepared_cleanup_accepts_missing_output_and_missing_merge_checkpoint() {
    let fixture = PreparedFixture::new();
    std::fs::remove_file(&fixture.candidate).unwrap();
    // A previous remove attempt may have retired part of the staging directory.
    std::fs::remove_file(fixture.parts.join("transfer.json")).unwrap();
    remove_prepared_output(&fixture.parts, &fixture.record).unwrap();
    std::fs::remove_file(fixture.parts.join("merge.json")).unwrap();
    remove_prepared_output(&fixture.parts, &fixture.record).unwrap();
    assert_eq!(std::fs::read(&fixture.neighbor).unwrap(), b"another job");
}

#[cfg(not(windows))]
#[test]
fn prepared_cleanup_without_handle_bound_delete_preserves_file_and_recovery() {
    let fixture = PreparedFixture::new();
    let error = remove_prepared_output(&fixture.parts, &fixture.record).unwrap_err();
    assert!(error.to_string().contains("unavailable on this platform"));
    fixture.assert_recoverable(&fixture.checkpoint);
}
