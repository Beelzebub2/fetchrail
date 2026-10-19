//! Explicitly enabled local crash breadcrumbs. No automatic upload and no source identity.
use crate::health::Report;
use serde::{Deserialize, Serialize};
use std::path::Path;
const FILE: &str = "diagnostic-history.json";
const TTL: i64 = 7 * 86400;
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
    let path = root.join(FILE);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 1024 * 1024 => (),
        _ => return Err("Unrecognized local diagnostic history was preserved; inspect the app data file before retrying.".into()),
    }
    let history: History = crate::state_store::read(&path)
        .map_err(|_| "Unrecognized or damaged local diagnostic history was preserved.")?;
    if history.schema != 1 || history.purpose != "local-download-health-v1" {
        return Err("Unrecognized local diagnostic history was preserved.".into());
    }
    Ok(Some(history))
}
pub fn load(root: &Path, now: i64) -> Result<Vec<Case>, String> {
    let mut cases = read_history(root)?
        .map(|history| history.cases)
        .unwrap_or_default();
    cases.retain(|case| case.saved_at > now - TTL && case.saved_at <= now);
    cases.sort_by_key(|case| std::cmp::Reverse(case.saved_at));
    cases.truncate(20);
    Ok(cases)
}
pub fn save(root: &Path, cases: &[Case]) -> Result<(), String> {
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
    if read_history(root)?.is_some() {
        match std::fs::remove_file(root.join(FILE)) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot delete local diagnostic history.".into()),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
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
