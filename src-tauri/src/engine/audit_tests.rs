use super::*;

fn record(destination: &Path) -> DownloadRecord {
    serde_json::from_value(json!({"id": Uuid::new_v4(), "url":"https://example.test/file?signature=private-token", "fileName":"file.bin", "destination":destination,
        "status":"merging", "totalBytes":7, "downloadedBytes":7, "speedBps":0, "etaSeconds":null, "connections":1, "error":null,
        "createdAt":Utc::now(), "finishedAt":null})).unwrap()
}

#[test]
fn dates_weak_and_malformed_etags_never_authorize_resume() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::LAST_MODIFIED,
        "Mon, 12 Oct 2026 12:00:00 GMT".parse().unwrap(),
    );
    headers.insert(
        header::DATE,
        "Mon, 12 Oct 2026 12:10:00 GMT".parse().unwrap(),
    );
    assert!(representation_validator(&headers).is_none());
    for value in ["W/\"v1\"", "not-quoted", "\"contains space\"", "\"a\"b\""] {
        headers.insert(header::ETAG, value.parse().unwrap());
        assert!(representation_validator(&headers).is_none(), "{value}");
    }
    for value in ["\"v1\"", "\"\""] {
        headers.insert(header::ETAG, value.parse().unwrap());
        assert_eq!(representation_validator(&headers), Some(value.into()));
    }
}

#[test]
fn structural_events_drop_old_progress_and_reject_late_snapshots() {
    let mut events = ProgressEvents::default();
    let mut item = record(Path::new("file.bin"));
    item.status = DownloadStatus::Downloading;
    events.structural(&item);
    events.progress(&item);
    assert_eq!(events.dirty.len(), 1);
    for status in [
        DownloadStatus::Paused,
        DownloadStatus::Completed,
        DownloadStatus::Cancelled,
    ] {
        let mut structural = item.clone();
        structural.status = status;
        events.structural(&structural);
        events.progress(&item);
        assert!(
            events.dirty.is_empty(),
            "Late Downloading events cannot regress status"
        );
    }
    let public = item.public_view();
    assert!(!public.url.contains("private-token"));
}

#[test]
fn integrity_and_authentication_failures_never_retry() {
    for error in [
        EngineError::Http(StatusCode::FORBIDDEN),
        EngineError::Message("SHA-256 checksum mismatch".into()),
        EngineError::Message("File changed while downloading".into()),
        EngineError::RangeUnsupported,
    ] {
        assert!(!failure_category(&error).1);
    }
    assert!(failure_category(&EngineError::Incomplete).1);
    assert!(failure_category(&EngineError::Http(StatusCode::SERVICE_UNAVAILABLE)).1);
}

#[tokio::test]
async fn publication_crash_stages_reconcile_exactly_one_file_and_reject_foreign_occupants() {
    // Five repetitions across four boundaries = 20 deterministic crash-state fixtures.
    for _ in 0..5 {
        for stage in 0..4 {
            let root = std::env::temp_dir().join(format!("fetchrail-publish-{}", Uuid::new_v4()));
            let output = root.join("file.bin");
            let mut saved = record(&output);
            let temp = root.join(format!(".file.bin.{}.fixture.fetchrail-part", saved.id));
            let part_dir = root.join("parts").join(saved.id.to_string());
            fs::create_dir_all(&part_dir).await.unwrap();
            fs::write(part_dir.join("transfer.json"), b"manifest")
                .await
                .unwrap();
            fs::write(&temp, b"payload").await.unwrap();
            let (identity, total, sha256) =
                file_fingerprint(&temp, &CancellationToken::new()).unwrap();
            let mut journal = PublicationJournal {
                version: 1,
                id: saved.id,
                destination: output.clone(),
                total,
                manifest_sha256: format!("{:x}", Sha256::digest(b"manifest")),
                sha256,
                identity,
                published: false,
            };
            write_json_atomic(
                &part_dir.join("merge.json"),
                &MergeCheckpoint {
                    file_name: temp.file_name().unwrap().to_str().unwrap().into(),
                    total,
                    fingerprint: journal.manifest_sha256.clone(),
                    expected_sha256: saved.expected_sha256.clone(),
                },
            )
            .await
            .unwrap();
            write_json_atomic(&part_dir.join("publication.json"), &journal)
                .await
                .unwrap();
            if stage > 0 {
                crate::platform::publish_noreplace(&temp, &output).unwrap();
            }
            if stage > 1 {
                journal.published = true;
                write_json_atomic(&part_dir.join("publication.json"), &journal)
                    .await
                    .unwrap();
            }
            if stage > 2 {
                saved.status = DownloadStatus::Completed;
            }
            assert_eq!(
                recover_publication(&part_dir, &mut saved).await.unwrap(),
                true
            );
            if stage > 0 {
                assert_eq!(saved.status, DownloadStatus::Completed);
                assert_eq!(fs::read(&output).await.unwrap(), b"payload");
                assert!(!temp.exists());
                // Identical bytes at a different file identity are still foreign.
                let foreign = root.join("foreign");
                fs::write(&foreign, b"payload").await.unwrap();
                fs::remove_file(&output).await.unwrap();
                fs::rename(&foreign, &output).await.unwrap();
                assert!(recover_publication(&part_dir, &mut saved).await.is_err());
                assert_eq!(fs::read(&output).await.unwrap(), b"payload");
            }
            fs::remove_dir_all(root).await.unwrap();
        }
    }
}

#[tokio::test]
async fn startup_gc_only_removes_owned_orphans_and_preserves_committed_or_unmarked_files() {
    let root = std::env::temp_dir().join(format!("fetchrail-gc-{}", Uuid::new_v4()));
    fs::create_dir_all(root.join("torrents")).await.unwrap();
    let committed = Uuid::new_v4();
    let orphan = Uuid::new_v4();
    let unmarked = Uuid::new_v4();
    fs::write(
        root.join("torrents").join(format!("{committed}.torrent")),
        b"keep",
    )
    .await
    .unwrap();
    fs::write(
        root.join("torrents").join(format!("{orphan}.torrent")),
        b"orphan",
    )
    .await
    .unwrap();
    fs::write(root.join("torrents").join("user.torrent"), b"user")
        .await
        .unwrap();
    let stage = root.join("torrent-imports").join(orphan.to_string());
    fs::create_dir_all(&stage).await.unwrap();
    write_json_atomic(&stage.join("owner.json"), &orphan)
        .await
        .unwrap();
    let unmarked_stage = root.join("torrent-imports").join(unmarked.to_string());
    fs::create_dir_all(&unmarked_stage).await.unwrap();
    gc_torrent_orphans(&root, &HashSet::from([committed]))
        .await
        .unwrap();
    assert!(root
        .join("torrents")
        .join(format!("{committed}.torrent"))
        .exists());
    assert!(!root
        .join("torrents")
        .join(format!("{orphan}.torrent"))
        .exists());
    assert!(root.join("torrents").join("user.torrent").exists());
    assert!(!stage.exists());
    assert!(unmarked_stage.exists());
    fs::remove_dir_all(root).await.unwrap();
}

#[tokio::test]
async fn manifest_scopes_resume_to_exact_url_without_persisting_signed_query_secrets() {
    let root = std::env::temp_dir().join(format!("fetchrail-url-scope-{}", Uuid::new_v4()));
    let mut probe = ProbeResult {
        total_bytes: Some(10),
        accepts_ranges: true,
        suggested_file_name: None,
        validator: Some("\"same-etag\"".into()),
        final_url: Some(Url::parse("https://example.test/file?signature=private-token").unwrap()),
    };
    let ranges = split_ranges(10, 1);
    prepare_parts(&root, &probe, &ranges).await.unwrap();
    let saved = fs::read_to_string(root.join("transfer.json"))
        .await
        .unwrap();
    assert!(!saved.contains("private-token"));
    assert!(!saved.contains("signature="));
    fs::write(root.join("0.part"), b"partial").await.unwrap();
    probe.final_url = Some(Url::parse("https://other.test/file?signature=other").unwrap());
    prepare_parts(&root, &probe, &ranges).await.unwrap();
    assert!(
        !root.join("0.part").exists(),
        "A same ETag on a different URL cannot authorize reuse"
    );
    fs::remove_dir_all(root).await.unwrap();
}

#[test]
fn publication_child_fault_fixture() {
    let Ok(root) = std::env::var("FETCHRAIL_PUBLICATION_CRASH_ROOT") else {
        return;
    };
    let stage: u8 = std::env::var("FETCHRAIL_PUBLICATION_CRASH_STAGE")
        .unwrap()
        .parse()
        .unwrap();
    let root = PathBuf::from(root);
    assert!(root.is_absolute());
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let output = root.join("file.bin");
            let mut saved = record(&output);
            let temporary = root.join(format!(".file.bin.{}.fixture.fetchrail-part", saved.id));
            let parts = root.join("parts").join(saved.id.to_string());
            fs::create_dir_all(&parts).await.unwrap();
            fs::write(parts.join("transfer.json"), b"manifest")
                .await
                .unwrap();
            fs::write(&temporary, b"payload").await.unwrap();
            crate::state_store::write(&root.join("catalog.json"), &saved).unwrap();
            let (identity, total, sha256) =
                file_fingerprint(&temporary, &CancellationToken::new()).unwrap();
            let mut journal = PublicationJournal {
                version: 1,
                id: saved.id,
                destination: output.clone(),
                total,
                manifest_sha256: format!("{:x}", Sha256::digest(b"manifest")),
                sha256,
                identity,
                published: false,
            };
            write_json_atomic(
                &parts.join("merge.json"),
                &MergeCheckpoint {
                    file_name: temporary.file_name().unwrap().to_str().unwrap().into(),
                    total,
                    fingerprint: journal.manifest_sha256.clone(),
                    expected_sha256: saved.expected_sha256.clone(),
                },
            )
            .await
            .unwrap();
            write_json_atomic(&parts.join("publication.json"), &journal)
                .await
                .unwrap();
            if stage >= 1 {
                crate::platform::publish_noreplace(&temporary, &output).unwrap();
            }
            if stage >= 2 {
                journal.published = true;
                write_json_atomic(&parts.join("publication.json"), &journal)
                    .await
                    .unwrap();
            }
            if stage >= 3 {
                recover_publication(&parts, &mut saved).await.unwrap();
                crate::state_store::write(&root.join("catalog.json"), &saved).unwrap();
            }
        });
    println!("READY_FOR_OWNED_PROCESS_KILL");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}

#[tokio::test]
async fn twenty_process_kills_leave_at_most_one_output_and_recover_durable_completion() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    for _ in 0..5 {
        for stage in 0..4 {
            let root =
                std::env::temp_dir().join(format!("fetchrail-kill-publish-{}", Uuid::new_v4()));
            let mut command = Command::new(std::env::current_exe().unwrap());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let mut child = command
                .args([
                    "--exact",
                    "engine::audit_tests::publication_child_fault_fixture",
                    "--nocapture",
                ])
                .env("FETCHRAIL_PUBLICATION_CRASH_ROOT", &root)
                .env("FETCHRAIL_PUBLICATION_CRASH_STAGE", stage.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let ready = BufReader::new(child.stdout.take().unwrap())
                .lines()
                .map_while(Result::ok)
                .any(|line| line == "READY_FOR_OWNED_PROCESS_KILL");
            let _ = child.kill();
            child.wait().unwrap();
            assert!(ready, "The owned child must reach the exact crash boundary");
            let mut saved: DownloadRecord =
                crate::state_store::read(&root.join("catalog.json")).unwrap();
            let part_dir = root.join("parts").join(saved.id.to_string());
            assert_eq!(
                recover_publication(&part_dir, &mut saved).await.unwrap(),
                true
            );
            if stage > 0 {
                assert_eq!(saved.status, DownloadStatus::Completed);
                assert_eq!(fs::read(root.join("file.bin")).await.unwrap(), b"payload");
            }
            assert!(!root.join("file (1).bin").exists());
            fs::remove_dir_all(root).await.unwrap();
        }
    }
}
