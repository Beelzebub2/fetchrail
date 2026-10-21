//! Explicitly enabled local crash breadcrumbs. No automatic upload and no source identity.
use crate::health::Report;
use serde::{Deserialize, Serialize};
use std::path::Path;
const FILE: &str = "diagnostic-history.json";
const TTL: i64 = 7 * 86400;
/// In-memory evidence of actual history writes in this process. Enabling the
/// setting or reading an older file must not invent a successful save.
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveStatus {
    pub last_successful_save_at: Option<i64>,
    pub error: Option<SaveError>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SaveError {
    Read,
    Save,
    Delete,
    Worker,
    Choice,
}
impl SaveError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Read => "Cannot read local diagnostic history. Existing files were preserved; inspect local storage and retry.",
            Self::Save => "Cannot save local diagnostic history. New evidence may remain only in memory; inspect local storage and retry.",
            Self::Delete => "Cannot delete all owned local diagnostic history. Some saved evidence may remain; inspect local storage and retry.",
            Self::Worker => "Local diagnostic history storage did not finish. The result is unknown; inspect saved history before retrying.",
            Self::Choice => "Cannot save the local history choice. Check the current setting and retry before restarting.",
        }
    }
}
impl SaveStatus {
    pub fn saved(&mut self, completed_at: i64) {
        self.last_successful_save_at = Some(completed_at);
        self.error = None;
    }
    pub fn failed(&mut self, error: SaveError) {
        self.error = Some(error);
    }
    pub fn read_succeeded(&mut self) {
        if self.error == Some(SaveError::Read) {
            self.error = None;
        }
    }
    pub fn cleared(&mut self) {
        *self = Self::default();
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: uuid::Uuid,
    pub saved_at: i64,
    pub report: Report,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct History {
    schema: u32,
    purpose: String,
    cases: Vec<Case>,
}
fn read_history(root: &Path) -> Result<Option<History>, String> {
    read_history_path(&root.join(FILE))
}
fn read_history_path(path: &Path) -> Result<Option<History>, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 1024 * 1024 => (),
        _ => return Err("Unrecognized local diagnostic history was preserved; inspect the app data file before retrying.".into()),
    }
    let history: History = crate::state_store::read(path)
        .map_err(|_| "Unrecognized or damaged local diagnostic history was preserved.")?;
    if history.schema != 1 || history.purpose != "local-download-health-v1" {
        return Err("Unrecognized local diagnostic history was preserved.".into());
    }
    Ok(Some(history))
}
// Ordinary write errors remove staging files, but a hard crash can leave one.
// History operations are serialized by the engine's settings update lock.
fn clear_temporary(root: &Path) -> Result<(), String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("Cannot inspect local diagnostic staging files.".into()),
    };
    let mut failure = None;
    for (index, entry) in entries.enumerate() {
        if index >= 4096 {
            return Err("Local diagnostic staging inspection exceeded its bound; inspect the app data before retrying.".into());
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                failure = Some("Cannot inspect local diagnostic staging files.".to_string());
                continue;
            }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(nonce) = name
            .strip_prefix("diagnostic-history.")
            .and_then(|name| name.strip_suffix(".tmp"))
        else {
            continue;
        };
        if !uuid::Uuid::parse_str(nonce).is_ok_and(|id| id.to_string() == nonce) {
            continue;
        }
        match read_history_path(&entry.path()) {
            Ok(Some(history))
                if history.cases.len() <= 20
                    && serde_json::to_vec(&history)
                        .is_ok_and(|bytes| bytes.len() <= 512 * 1024) =>
            {
                if let Err(error) = std::fs::remove_file(entry.path()) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        failure =
                            Some("Cannot delete an owned local diagnostic staging file.".into());
                    }
                }
            }
            Ok(None) => (),
            _ => {
                failure = Some("Unrecognized or damaged local diagnostic staging file was preserved; inspect the app data before retrying.".into());
            }
        }
    }
    failure.map_or(Ok(()), Err)
}
pub fn load(root: &Path, now: i64) -> Result<Vec<Case>, String> {
    clear_temporary(root)?;
    let mut cases = read_history(root)?
        .map(|history| history.cases)
        .unwrap_or_default();
    cases.retain(|case| case.saved_at > now - TTL && case.saved_at <= now);
    cases.sort_by_key(|case| std::cmp::Reverse(case.saved_at));
    cases.truncate(20);
    Ok(cases)
}
pub fn save(root: &Path, cases: &[Case]) -> Result<(), String> {
    clear_temporary(root)?;
    read_history(root)?;
    if cases.len() > 20 {
        return Err("Local diagnostic history limit reached.".into());
    }
    let mut history = History {
        schema: 1,
        purpose: "local-download-health-v1".into(),
        cases: cases.to_vec(),
    };
    let bytes = loop {
        let bytes =
            serde_json::to_vec(&history).map_err(|_| "Cannot encode local diagnostic history.")?;
        if bytes.len() <= 512 * 1024 {
            break bytes;
        }
        history.cases.pop();
    };
    // No backup generation: expired or explicitly cleared cases must not survive in a previous copy.
    crate::platform::write_private_atomic(&root.join(FILE), &crate::state_store::encode(&bytes)?)
        .map_err(|_| "Cannot save local diagnostic history.".into())
}
pub fn clear(root: &Path) -> Result<(), String> {
    let main = read_history(root).and_then(|history| {
        if history.is_some() {
            match std::fs::remove_file(root.join(FILE)) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(_) => return Err("Cannot delete local diagnostic history.".into()),
            }
        }
        Ok(())
    });
    // A foreign primary file must not prevent clearing recognized crash leftovers.
    let temporary = clear_temporary(root);
    main.and(temporary)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn save_status_requires_a_successful_write_preserves_failures_and_clears_after_deletion() {
        let mut status = SaveStatus::default();
        assert_eq!(status.last_successful_save_at, None);
        status.read_succeeded();
        assert_eq!(
            status.last_successful_save_at, None,
            "Inspecting an old file is not a new save"
        );
        status.failed(SaveError::Save);
        status.read_succeeded();
        assert_eq!(
            status.error,
            Some(SaveError::Save),
            "A successful read cannot conceal a failed save"
        );
        status.saved(100);
        assert_eq!(status.last_successful_save_at, Some(100));
        assert_eq!(status.error, None);
        for error in [
            SaveError::Read,
            SaveError::Save,
            SaveError::Delete,
            SaveError::Worker,
            SaveError::Choice,
        ] {
            status.failed(error);
            assert_eq!(status.last_successful_save_at, Some(100));
            assert_eq!(status.error, Some(error));
        }
        status.failed(SaveError::Read);
        status.read_succeeded();
        assert_eq!(status.error, None);
        status.saved(200);
        status.cleared();
        assert_eq!(status.last_successful_save_at, None);
        assert_eq!(status.error, None);
        let encoded = serde_json::to_string(&status).unwrap();
        assert_eq!(encoded, r#"{"lastSuccessfulSaveAt":null,"error":null}"#);
        assert!(encoded.len() < 128);
    }
    fn temporary_path(root: &Path) -> std::path::PathBuf {
        root.join(FILE)
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()))
    }
    #[test]
    fn owned_crash_leftovers_are_cleared_on_load_save_and_withdrawal() {
        let root = std::env::temp_dir().join(format!(
            "fetchrail-history-leftovers-{}",
            uuid::Uuid::new_v4()
        ));
        save(
            &root,
            &[Case {
                id: uuid::Uuid::new_v4(),
                saved_at: 1_000_000,
                report: crate::health::Health::default().snapshot(false),
            }],
        )
        .unwrap();
        let snapshot = std::fs::read(root.join(FILE)).unwrap();
        std::fs::remove_file(root.join(FILE)).unwrap();
        let unrelated = root.join("diagnostic-history.personal.tmp");
        std::fs::write(&unrelated, &snapshot).unwrap();

        let orphan = temporary_path(&root);
        std::fs::write(&orphan, &snapshot).unwrap();
        assert!(load(&root, 1_000_001).unwrap().is_empty());
        assert!(!orphan.exists()); // An uncommitted snapshot is never recovered.
        assert_eq!(std::fs::read(&unrelated).unwrap(), snapshot);

        let orphan = temporary_path(&root);
        std::fs::write(&orphan, &snapshot).unwrap();
        save(&root, &[]).unwrap();
        assert!(!orphan.exists());

        let orphan = temporary_path(&root);
        std::fs::write(&orphan, &snapshot).unwrap();
        clear(&root).unwrap();
        assert!(!root.join(FILE).exists());
        assert!(!orphan.exists());
        assert!(unrelated.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn damaged_or_foreign_files_are_preserved_without_blocking_owned_cleanup() {
        let root = std::env::temp_dir().join(format!(
            "fetchrail-history-foreign-{}",
            uuid::Uuid::new_v4()
        ));
        save(&root, &[]).unwrap();
        let owned = temporary_path(&root);
        std::fs::copy(root.join(FILE), &owned).unwrap();
        let damaged = temporary_path(&root);
        std::fs::write(&damaged, b"partial crash write").unwrap();
        let foreign = temporary_path(&root);
        let foreign_bytes =
            crate::state_store::encode(br#"{"schema":1,"purpose":"personal-notes","cases":[]}"#)
                .unwrap();
        std::fs::write(&foreign, &foreign_bytes).unwrap();
        std::fs::write(root.join(FILE), b"personal primary file").unwrap();

        assert!(clear(&root).is_err());
        assert!(!owned.exists());
        assert_eq!(std::fs::read(&damaged).unwrap(), b"partial crash write");
        assert_eq!(std::fs::read(&foreign).unwrap(), foreign_bytes);
        assert_eq!(
            std::fs::read(root.join(FILE)).unwrap(),
            b"personal primary file"
        );
        assert!(save(&root, &[]).is_err());
        assert!(load(&root, 1_000_000).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn staging_symlinks_are_preserved_even_when_the_target_is_owned_history() {
        let root = std::env::temp_dir().join(format!(
            "fetchrail-history-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        save(&root, &[]).unwrap();
        let target = root.join("personal-copy.json");
        std::fs::copy(root.join(FILE), &target).unwrap();
        let before = std::fs::read(&target).unwrap();
        let link = temporary_path(&root);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(clear(&root).is_err());
        assert!(!root.join(FILE).exists());
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&target).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn crash_fixture_child() {
        let Some(root) = std::env::var_os("FETCHRAIL_HEALTH_CRASH_FIXTURE") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let health = crate::health::Health::default();
        health.accepted();
        health.cause(crate::health::Cause::Http503);
        if std::env::var_os("FETCHRAIL_HEALTH_SAVE_FIXTURE").is_some() {
            save(
                &root,
                &[Case {
                    id: uuid::Uuid::new_v4(),
                    saved_at: 1_000_000,
                    report: health.snapshot(false),
                }],
            )
            .unwrap();
        }
        health.completed(123); // Intentionally lost: no final breadcrumb is saved.
        std::process::exit(73);
    }
    #[test]
    fn abrupt_exit_loses_unsaved_evidence_and_never_fabricates_final_completion() {
        for persist in [false, true] {
            let root = std::env::temp_dir()
                .join(format!("fetchrail-health-crash-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "health_history::tests::crash_fixture_child"])
                .env("FETCHRAIL_HEALTH_CRASH_FIXTURE", &root)
                .env_remove("FETCHRAIL_HEALTH_SAVE_FIXTURE");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0800_0000);
            }
            if persist {
                command.env("FETCHRAIL_HEALTH_SAVE_FIXTURE", "1");
            }
            assert_eq!(command.status().unwrap().code(), Some(73));
            let cases = load(&root, 1_000_001).unwrap();
            assert_eq!(cases.len(), usize::from(persist));
            assert!(cases
                .iter()
                .all(|case| case.report.durable_completed_ms.is_none()));
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn protection_expiry_bounds_deletion_and_foreign_file_preservation() {
        let root =
            std::env::temp_dir().join(format!("fetchrail-breadcrumb-{}", uuid::Uuid::new_v4()));
        let now = 1_000_000;
        let cases: Vec<_> = (0..20)
            .map(|i| Case {
                id: uuid::Uuid::new_v4(),
                saved_at: now - i * 86400,
                report: crate::health::Health::default().snapshot(false),
            })
            .collect();
        save(&root, &cases).unwrap();
        assert_eq!(load(&root, now).unwrap().len(), 7);
        save(&root, &load(&root, now).unwrap()).unwrap();
        assert!(load(&root, now + TTL).unwrap().is_empty());
        clear(&root).unwrap();
        assert!(!root.join(FILE).exists());
        std::fs::write(root.join(FILE), b"personal file; do not replace or delete").unwrap();
        assert!(save(&root, &[]).is_err());
        assert!(clear(&root).is_err());
        assert_eq!(
            std::fs::read(root.join(FILE)).unwrap(),
            b"personal file; do not replace or delete"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
