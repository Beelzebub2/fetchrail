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
                range_sha256: Vec::new(),
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
                    identity: None,
                },
            )
            .await
            .unwrap();
            write_json_atomic(&part_dir.join("publication.json"), &journal)
                .await
                .unwrap();
            if stage > 0 {
                let (_, _, file) =
                    open_publication_file_with_access(&temp, PublicationAccess::Publish).unwrap();
                let publication = crate::platform::publish_verified_noreplace(
                    &file,
                    &temp,
                    &output,
                    #[cfg(feature = "test-tools")]
                    None,
                )
                .unwrap();
                // Model process exit after publication, before its journal/catalog acknowledgment.
                drop(file);
                drop(publication);
                assert!(!temp.exists());
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
async fn shared_range_commitments_recover_without_an_initial_full_file_digest() {
    use std::io::{Seek, SeekFrom};

    let root = std::env::temp_dir().join(format!("fetchrail-range-journal-{}", Uuid::new_v4()));
    let destination = root.join("file.bin");
    let mut saved = record(&destination);
    let parts = root.join("parts").join(saved.id.to_string());
    fs::create_dir_all(&parts).await.unwrap();
    let data = (0..64 * 1024).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let total = data.len() as u64;
    let ranges = split_ranges(total, 4);
    let manifest = PartManifest {
        total_bytes: Some(total),
        ranges: ranges.clone(),
        validator: Some("\"checked-representation\"".into()),
        version: 1,
        final_url_sha256: Some("0".repeat(64)),
        accepts_ranges: true,
        shared_staging: true,
    };
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(parts.join("transfer.json"), &manifest_bytes)
        .await
        .unwrap();
    let stage = parts.join("0.part");
    fs::write(&stage, &data).await.unwrap();
    for (index, range) in ranges.iter().enumerate() {
        let slice = &data[range.start as usize..=range.end as usize];
        write_json_atomic(
            &parts.join(format!("{index}.progress.json")),
            &PartCheckpoint {
                bytes: range.len(),
                sha256: format!("{:x}", Sha256::digest(slice)),
            },
        )
        .await
        .unwrap();
    }
    let hashes = completed_shared_hashes(&parts, total)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hashes.len(), 4);
    let temp = root.join(format!(".file.bin.{}.fixture.fetchrail-part", saved.id));
    std::fs::hard_link(&stage, &temp).unwrap();
    let (identity, bytes, _) = open_publication_file(&temp).unwrap();
    assert_eq!(bytes, total);
    assert_eq!(open_publication_file(&stage).unwrap().0, identity);
    let journal = PublicationJournal {
        version: 2,
        id: saved.id,
        destination: destination.clone(),
        total,
        manifest_sha256: format!("{:x}", Sha256::digest(&manifest_bytes)),
        sha256: String::new(),
        range_sha256: hashes,
        identity,
        published: false,
    };
    // A freshly synced cross-volume copy can carry the same commitments,
    // even though its file identity differs from the private source.
    let copied = root.join("separate-copy.fetchrail-part");
    assert_eq!(fs::copy(&stage, &copied).await.unwrap(), total);
    let (copy_identity, copy_len, _) = open_publication_file(&copied).unwrap();
    assert_eq!(copy_len, total);
    assert_ne!(copy_identity, journal.identity);
    let copied_journal = PublicationJournal {
        identity: copy_identity,
        ..journal.clone()
    };
    assert!(journal_file_matches(&copied, &copied_journal, &manifest_bytes, None).unwrap());
    let mut damaged_copy = std::fs::OpenOptions::new()
        .write(true)
        .open(&copied)
        .unwrap();
    damaged_copy.seek(SeekFrom::Start(total / 3)).unwrap();
    damaged_copy.write_all(b"damage").unwrap();
    damaged_copy.sync_all().unwrap();
    drop(damaged_copy);
    assert!(!journal_file_matches(&copied, &copied_journal, &manifest_bytes, None).unwrap());
    fs::remove_file(copied).await.unwrap();
    assert!(journal_file_matches(&temp, &journal, &manifest_bytes, None).unwrap());
    assert!(
        !journal_file_matches(&temp, &journal, &manifest_bytes, Some(&"0".repeat(64))).unwrap()
    );
    write_json_atomic(
        &parts.join("merge.json"),
        &MergeCheckpoint {
            file_name: temp.file_name().unwrap().to_str().unwrap().into(),
            total,
            fingerprint: journal.manifest_sha256.clone(),
            expected_sha256: None,
            identity: None,
        },
    )
    .await
    .unwrap();
    write_json_atomic(&parts.join("publication.json"), &journal)
        .await
        .unwrap();
    assert!(recover_publication(&parts, &mut saved).await.unwrap());
    assert_eq!(saved.status, DownloadStatus::Completed);
    assert_eq!(fs::read(&destination).await.unwrap(), data);

    // An in-place modification keeps the same file identity and length. The
    // persisted segment hashes must still reject it on the next recovery.
    let mut changed = std::fs::OpenOptions::new()
        .write(true)
        .open(&destination)
        .unwrap();
    changed.seek(SeekFrom::Start(total / 2)).unwrap();
    changed.write_all(b"corrupted").unwrap();
    changed.sync_all().unwrap();
    drop(changed);
    assert!(recover_publication(&parts, &mut saved).await.is_err());
    fs::remove_dir_all(root).await.unwrap();
}

#[test]
fn independent_parallel_range_handles_reject_truncation_and_mutation() {
    use std::io::{Seek, SeekFrom};

    let root = std::env::temp_dir().join(format!("fetchrail-parallel-sha-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("staged.bin");
    let bytes = (0..1024 * 1024)
        .map(|i| (i / (64 * 1024)) as u8)
        .collect::<Vec<_>>();
    std::fs::write(&path, &bytes).unwrap();
    let ranges = split_ranges(bytes.len() as u64, 16);
    let hashes = ranges
        .iter()
        .map(|range| {
            format!(
                "{:x}",
                Sha256::digest(&bytes[range.start as usize..=range.end as usize])
            )
        })
        .collect::<Vec<_>>();
    let cancel = CancellationToken::new();
    let (identity, _, mut file) =
        open_publication_file_with_access(&path, PublicationAccess::Publish).unwrap();
    assert!(range_hashes_match(
        file.try_clone().unwrap(),
        &path,
        &identity,
        &ranges,
        &hashes,
        &cancel,
        1
    )
    .unwrap());
    file.seek(SeekFrom::Start(31)).unwrap();
    assert!(range_hashes_match(
        file.try_clone().unwrap(),
        &path,
        &identity,
        &ranges,
        &hashes,
        &cancel,
        8
    )
    .unwrap());
    assert_eq!(
        file.stream_position().unwrap(),
        31,
        "Parallel readers must have their own Windows file objects and cursors"
    );
    cancel.cancel();
    assert!(range_hashes_match(
        file.try_clone().unwrap(),
        &path,
        &identity,
        &ranges,
        &hashes,
        &cancel,
        4
    )
    .is_err());
    drop(file);

    let mut changed = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    changed
        .seek(SeekFrom::Start(ranges[8].start + 123))
        .unwrap();
    changed.write_all(b"tampered").unwrap();
    changed.sync_all().unwrap();
    drop(changed);
    let (identity, _, file) =
        open_publication_file_with_access(&path, PublicationAccess::Publish).unwrap();
    assert!(!range_hashes_match(
        file,
        &path,
        &identity,
        &ranges,
        &hashes,
        &CancellationToken::new(),
        4
    )
    .unwrap());
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(128 * 1024)
        .unwrap();
    let (identity, _, file) =
        open_publication_file_with_access(&path, PublicationAccess::Publish).unwrap();
    assert!(!range_hashes_match(
        file,
        &path,
        &identity,
        &ranges,
        &hashes,
        &CancellationToken::new(),
        4
    )
    .unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn protected_verification_worker_limit_scales_without_unbounded_buffers() {
    const MIB: u64 = 1024 * 1024;
    assert_eq!(postscan_verification_workers(127 * MIB, 16), 1);
    assert_eq!(postscan_verification_workers(128 * MIB, 16), 4);
    assert_eq!(postscan_verification_workers(1023 * MIB, 16), 4);
    assert_eq!(postscan_verification_workers(1024 * MIB, 16), 8);
    assert_eq!(postscan_verification_workers(20 * 1024 * MIB, 16), 8);
    assert_eq!(postscan_verification_workers(20 * 1024 * MIB, 6), 6);
    assert_eq!(postscan_verification_workers(20 * 1024 * MIB, 0), 1);
}

#[test]
#[ignore = "local-only postscan hashing benchmark; not part of routine tests"]
fn benchmark_protected_range_hashes_serial_and_parallel() {
    let scratch = std::env::var_os("FETCHRAIL_SHA_BENCH_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    assert!(scratch.is_dir(), "Benchmark scratch directory must exist");
    let root = scratch.join(format!("fetchrail-sha-bench-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("synthetic-staging.bin");
    let repeats = std::env::var("FETCHRAIL_SHA_BENCH_REPEATS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|count| [32, 64, 128].contains(count))
        .unwrap_or(32);
    let payload = (0..8 * 1024 * 1024)
        .map(|i| (i / 4096) as u8)
        .collect::<Vec<_>>();
    let mut output = std::fs::File::create(&path).unwrap();
    for _ in 0..repeats {
        output.write_all(&payload).unwrap();
    }
    output.sync_all().unwrap();
    drop(output);
    let mut hasher = Sha256::new();
    for _ in 0..(repeats / 16) {
        hasher.update(&payload);
    }
    let expected = format!("{:x}", hasher.finalize());
    let (identity, total, protected) =
        open_publication_file_with_access(&path, PublicationAccess::Publish).unwrap();
    let ranges = split_ranges(total, 16);
    let hashes = vec![expected; 16];
    let mut times = Vec::new();
    let worker_plan = std::env::var("FETCHRAIL_SHA_BENCH_WORKERS")
        .ok()
        .and_then(|value| {
            value
                .split(',')
                .map(str::parse::<usize>)
                .collect::<Result<Vec<_>, _>>()
                .ok()
        })
        .filter(|workers| {
            !workers.is_empty()
                && workers.len() <= 12
                && workers.iter().all(|&worker| (1..=16).contains(&worker))
        })
        .unwrap_or_else(|| vec![1, 4, 8, 6, 8, 4, 1]);
    for workers in worker_plan {
        let started = Instant::now();
        assert!(range_hashes_match(
            protected.try_clone().unwrap(),
            &path,
            &identity,
            &ranges,
            &hashes,
            &CancellationToken::new(),
            workers
        )
        .unwrap());
        times.push((workers, started.elapsed().as_millis()));
    }
    println!(
        "Protected {}MiB range SHA-256 [workers, elapsed_ms] = {times:?}",
        repeats * 8
    );
    drop(protected);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(all(windows, feature = "test-tools"))]
#[tokio::test]
#[ignore = "opt-in isolated synthetic Windows attachment and publish timing; never user files"]
async fn benchmark_synthetic_windows_attachment_and_publish() {
    use std::io::Write;
    // This diagnostic intentionally tests the exact Windows APIs used in a
    // publication, not actual source data. Use a brand-new, private test file;
    // never invoke this test on an existing download or user-supplied path.
    let scratch = PathBuf::from(
        std::env::var_os("FETCHRAIL_SCAN_BENCH_ROOT")
            .expect("Set FETCHRAIL_SCAN_BENCH_ROOT to an existing isolated scratch directory"),
    );
    assert!(
        scratch.is_absolute(),
        "Synthetic scratch path must be absolute"
    );
    assert!(
        scratch.is_dir(),
        "Isolated scan benchmark directory must exist"
    );
    for ancestor in scratch.ancestors() {
        use std::os::windows::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(ancestor).unwrap();
        assert_eq!(
            metadata.file_attributes() & 0x400,
            0,
            "Synthetic scratch path must not contain reparse points"
        );
    }
    let gib = std::env::var("FETCHRAIL_SCAN_BENCH_GIB")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|size| [1, 4, 20].contains(size))
        .expect("Set FETCHRAIL_SCAN_BENCH_GIB to 1, 4, or 20");
    let root = scratch.join(format!("fetchrail-synthetic-scan-{}", Uuid::new_v4()));
    let length = gib * 1024 * 1024 * 1024;
    let available = crate::platform::available_space(&scratch).unwrap();
    assert!(
        available >= length + 4 * 1024 * 1024 * 1024,
        "Insufficient free space for isolated synthetic benchmark"
    );
    std::fs::create_dir(&root).unwrap();
    struct OwnedFixtureDir(PathBuf);
    impl Drop for OwnedFixtureDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _owned_cleanup = OwnedFixtureDir(root.clone());
    let staging = root.join(".fetchrail-owned-synthetic.fetchrail-part");
    let extension = if std::env::var("FETCHRAIL_SCAN_BENCH_EXTENSION").as_deref() == Ok("bin") {
        "bin"
    } else {
        "rar"
    };
    let filename = format!("fetchrail-owned-synthetic.{extension}");
    let target = root.join(&filename);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .unwrap();
    let populated = std::env::var("FETCHRAIL_SCAN_BENCH_POPULATE").as_deref() == Ok("1");
    let internet_source = std::env::var("FETCHRAIL_SCAN_BENCH_SOURCE").as_deref() == Ok("internet");
    let populate_started = Instant::now();
    if populated {
        let mut buffer = vec![0u8; 8 * 1024 * 1024];
        let mut x = 0x9e37_79b9_u32;
        for byte in &mut buffer {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *byte = x as u8;
        }
        for _ in 0..length / buffer.len() as u64 {
            file.write_all(&buffer).unwrap();
        }
        assert_eq!(file.metadata().unwrap().len(), length);
    } else {
        file.set_len(length).unwrap();
    }
    file.sync_all().unwrap();
    drop(file);
    let populate_ms = populate_started.elapsed().as_millis();

    let trace = std::sync::Arc::new(trace::Trace::default());
    let scan_started = Instant::now();
    let source = if internet_source {
        // Source metadata only. This test never performs an HTTP request.
        format!("https://example.com/{filename}")
    } else {
        format!("http://127.0.0.1/{filename}")
    };
    crate::platform::mark_download(&staging, &source, &filename, Some(trace.clone()))
        .await
        .unwrap();
    let attachment_ms = scan_started.elapsed().as_millis();
    let (_, actual, protected) =
        open_publication_file_with_access(&staging, PublicationAccess::Publish).unwrap();
    assert_eq!(
        actual, length,
        "Synthetic file was changed by attachment policy"
    );
    let publish_started = Instant::now();
    let publication_guard = crate::platform::publish_verified_noreplace(
        &protected,
        &staging,
        &target,
        Some(trace.as_ref()),
    )
    .unwrap();
    let publish_ms = publish_started.elapsed().as_millis();
    let spans = trace.snapshot();
    assert_eq!(spans["spans"]["attachmentSave"]["calls"], 1);
    assert_eq!(spans["spans"]["publishLink"]["calls"], 1);
    assert!(spans["spans"]["publishRename"]["calls"].as_u64().unwrap() <= 1);
    let save_ms = spans["spans"]["attachmentSave"]["totalMs"]
        .as_f64()
        .unwrap();
    let rename_ms = spans["spans"]["publishRename"]["totalMs"].as_f64().unwrap();
    assert_eq!(std::fs::metadata(&target).unwrap().len(), length);
    drop(protected);
    drop(publication_guard);
    assert!(
        !staging.exists(),
        "Publication left its temporary file behind"
    );
    println!(
        "Synthetic {}GiB extension={} populated={} Internet-source={} create+sync={}ms, COM Save={:.3}ms (wrapper={}ms), no-clobber rename={:.3}ms (wrapper={}ms); no scanner attribution",
        gib, extension, populated, internet_source, populate_ms, save_ms, attachment_ms, rename_ms, publish_ms
    );
}

#[cfg(windows)]
#[test]
fn handle_link_publication_pins_bytes_and_retires_only_the_temporary_name() {
    let root =
        std::env::temp_dir().join(format!("fetchrail-linked-publication-{}", Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let source = root.join("temporary.fetchrail-part");
    let destination = root.join("published-Ω.bin");
    let bytes = b"verified publication with a surviving final name";
    std::fs::write(&source, bytes).unwrap();
    let mut ads = source.as_os_str().to_os_string();
    ads.push(":Zone.Identifier");
    std::fs::write(&ads, b"[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
    let (identity, _, file) =
        open_publication_file_with_access(&source, PublicationAccess::Publish).unwrap();
    #[cfg(feature = "test-tools")]
    let trace = trace::Trace::default();
    let publication_guard = crate::platform::publish_verified_noreplace(
        &file,
        &source,
        &destination,
        #[cfg(feature = "test-tools")]
        Some(&trace),
    )
    .unwrap();
    #[cfg(feature = "test-tools")]
    {
        let spans = trace.snapshot();
        assert_eq!(spans["spans"]["publishLink"]["calls"], 1);
        assert_eq!(spans["spans"]["publishRename"]["calls"], 0);
        assert_eq!(spans["spans"]["publishRetire"]["calls"], 1);
    }
    assert_eq!(open_publication_file(&destination).unwrap().0, identity);
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&destination)
        .is_err());
    assert!(std::fs::rename(&destination, root.join("unexpected.bin")).is_err());
    // The last duplicate holds the guard through both durable catalog writes.
    let duplicate = file.try_clone().unwrap();
    drop(file);
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&destination)
        .is_err());
    drop(duplicate);
    assert!(std::fs::rename(&destination, root.join("unexpected.bin")).is_err());
    drop(publication_guard);
    assert!(!source.exists());
    assert_eq!(std::fs::read(&destination).unwrap(), bytes);
    let mut published_ads = destination.as_os_str().to_os_string();
    published_ads.push(":Zone.Identifier");
    assert_eq!(
        std::fs::read(published_ads).unwrap(),
        b"[ZoneTransfer]\r\nZoneId=3\r\n"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn post_attachment_verification_rejects_mutations_and_keeps_provenance() {
    use std::io::{Seek, SeekFrom};
    for whole_hash in [false, true] {
        for copied in [false, true] {
            for outcome in [
                "unchanged",
                "same-size",
                "truncate",
                "delete",
                "replace",
                "ads",
                "fail",
            ] {
                let root = std::env::temp_dir().join(format!("fetchrail-scan-{}", Uuid::new_v4()));
                fs::create_dir_all(&root).await.unwrap();
                let stage = root.join("0.part");
                let temp = root.join("scanned.part");
                let output = root.join("published-Ω-file.bin");
                let data = (0..64 * 1024).map(|i| (i % 251) as u8).collect::<Vec<_>>();
                fs::write(&stage, &data).await.unwrap();
                if copied {
                    fs::copy(&stage, &temp).await.unwrap();
                } else {
                    std::fs::hard_link(&stage, &temp).unwrap();
                }
                let total = data.len() as u64;
                let ranges = split_ranges(total, 4);
                let manifest = PartManifest {
                    total_bytes: Some(total),
                    ranges: ranges.clone(),
                    validator: Some("\"source\"".into()),
                    version: 1,
                    final_url_sha256: Some("0".repeat(64)),
                    accepts_ranges: true,
                    shared_staging: true,
                };
                let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
                let hashes = ranges
                    .iter()
                    .map(|range| {
                        format!(
                            "{:x}",
                            Sha256::digest(&data[range.start as usize..=range.end as usize])
                        )
                    })
                    .collect::<Vec<_>>();
                let identity = open_publication_file(&temp).unwrap().0;
                // The controlled scanner runs before the same verifier used by real publication.
                let scan = (|| -> std::io::Result<()> {
                    match outcome {
                        "same-size" => {
                            let mut file = std::fs::OpenOptions::new().write(true).open(&temp)?;
                            file.seek(SeekFrom::Start(total / 2))?;
                            file.write_all(b"changed bytes")?;
                            file.sync_all()?;
                            drop(file);
                            let (after_identity, after_len, _) = open_publication_file(&temp)?;
                            assert_eq!(after_identity, identity);
                            assert_eq!(after_len, total);
                        }
                        "truncate" => std::fs::OpenOptions::new()
                            .write(true)
                            .open(&temp)?
                            .set_len(total / 2)?,
                        "delete" => std::fs::remove_file(&temp)?,
                        "replace" => {
                            let replacement = root.join("replacement");
                            std::fs::write(&replacement, &data)?;
                            std::fs::remove_file(&temp)?;
                            std::fs::rename(replacement, &temp)?;
                        }
                        "ads" => {
                            #[cfg(windows)]
                            {
                                let mut ads = temp.as_os_str().to_os_string();
                                ads.push(":Zone.Identifier");
                                std::fs::write(ads, b"[ZoneTransfer]\r\nZoneId=3\r\n")?;
                            }
                        }
                        "fail" => return Err(std::io::ErrorKind::PermissionDenied.into()),
                        _ => {}
                    }
                    Ok(())
                })();
                let result = scan.map_err(EngineError::from).and_then(|()| {
                    verify_scanned_publication(
                        &temp,
                        &identity,
                        total,
                        &manifest_bytes,
                        (!whole_hash).then_some(hashes),
                        whole_hash
                            .then(|| format!("{:x}", Sha256::digest(&data)))
                            .as_deref(),
                        &CancellationToken::new(),
                    )
                });
                if matches!(outcome, "unchanged" | "ads") {
                    let verified = result.unwrap();
                    #[cfg(windows)]
                    assert!(
                        std::fs::OpenOptions::new().write(true).open(&temp).is_err(),
                        "A verified file stays protected until catalog acknowledgement"
                    );
                    #[cfg(windows)]
                    assert!(
                        std::fs::rename(&temp, root.join("swapped-name")).is_err(),
                        "A verifier must prevent deletion/replacement through another handle"
                    );
                    fs::write(&output, b"foreign output").await.unwrap();
                    assert!(crate::platform::publish_verified_noreplace(
                        &verified.file,
                        &temp,
                        &output,
                        #[cfg(feature = "test-tools")]
                        None,
                    )
                    .is_err());
                    assert_eq!(fs::read(&output).await.unwrap(), b"foreign output");
                    fs::remove_file(&output).await.unwrap();
                    let publication_guard = crate::platform::publish_verified_noreplace(
                        &verified.file,
                        &temp,
                        &output,
                        #[cfg(feature = "test-tools")]
                        None,
                    )
                    .unwrap();
                    #[cfg(windows)]
                    assert!(std::fs::rename(&output, root.join("unexpected.bin")).is_err());
                    assert_eq!(fs::read(&output).await.unwrap(), data);
                    #[cfg(windows)]
                    if outcome == "ads" {
                        let mut ads = output.as_os_str().to_os_string();
                        ads.push(":Zone.Identifier");
                        assert!(std::fs::read(ads)
                            .unwrap()
                            .windows(8)
                            .any(|v| v == b"ZoneId=3"));
                    }
                    drop(verified);
                    drop(publication_guard);
                } else {
                    assert!(result.is_err(), "{outcome}, copied={copied}");
                    assert!(
                        !output.exists(),
                        "Changed or rejected files cannot be published"
                    );
                }
                assert!(
                    stage.exists(),
                    "A failed scan must preserve owned staging for recovery"
                );
                if copied {
                    assert_eq!(fs::read(stage).await.unwrap(), data);
                }
                fs::remove_dir_all(root).await.unwrap();
            }
        }
    }
}

#[test]
fn grouped_progress_preserves_bytes_and_does_not_equate_sections_with_connections() {
    let parts = (0..512)
        .map(|index| SegmentProgress {
            start: index * 1024,
            length: Some(1024),
            downloaded_bytes: index,
            speed_bps: if index < 16 { 1000 } else { 0 },
            active: index < 16,
            waiting: index >= 16,
        })
        .collect::<Vec<_>>();
    let downloaded = parts.iter().map(|part| part.downloaded_bytes).sum::<u64>();
    let grouped = compact_segments(parts);
    assert_eq!(grouped.len(), 32);
    assert_eq!(
        grouped
            .iter()
            .map(|part| part.downloaded_bytes)
            .sum::<u64>(),
        downloaded
    );
    assert_eq!(
        grouped.iter().map(|part| part.speed_bps).sum::<u64>(),
        16000
    );
    assert_eq!(
        grouped.iter().map(|part| part.length.unwrap()).sum::<u64>(),
        512 * 1024
    );
    assert_eq!(grouped.iter().filter(|part| part.active).count(), 1);
    assert_eq!(
        grouped.last().unwrap().start + grouped.last().unwrap().length.unwrap(),
        512 * 1024
    );
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
                range_sha256: Vec::new(),
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
                    identity: None,
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
