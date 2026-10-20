//! Durable negative receipts distinguish rejection from a validator still running.
//! This journal contains opaque identifiers and payload digests, never browser headers.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};
use tokio::sync::Mutex;

#[derive(Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub state: String,
    pub digest: Option<String>,
    pub updated: i64,
    #[serde(default)]
    pub ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pending_abort_and_commit_receipts_survive_restart_without_private_payloads() {
        let root = std::env::temp_dir().join(format!("fetchrail-handoff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("journal.json");
        let journal = Handoffs::load(path.clone()).unwrap();
        {
            let mut saved = journal.0.lock().await;
            saved
                .set("pending", "pending", Some("digest-only".into()))
                .unwrap();
            saved.set("abort", "aborted", None).unwrap();
            saved
                .set("commit", "staged", Some("digest-only".into()))
                .unwrap();
            saved
                .commit("commit", vec!["opaque-record".into()])
                .unwrap();
            assert_eq!(saved.result("pending")["state"], "pending");
            assert_eq!(saved.result("absent")["state"], "unknown");
        }
        let reloaded = Handoffs::load(path.clone()).unwrap();
        let mut saved = reloaded.0.lock().await;
        assert_eq!(saved.result("pending")["state"], "aborted");
        assert_eq!(saved.result("abort")["state"], "aborted");
        assert_eq!(saved.result("commit")["state"], "committed");
        assert_eq!(saved.result("commit")["ids"], json!(["opaque-record"]));
        assert_eq!(saved.entries["pending"].state, "interrupted");
        let disk = std::fs::read_to_string(&path).unwrap();
        assert!(!disk.contains("https://") && !disk.contains("cookie"));
        let moved = root.join("saved.json");
        std::fs::rename(&path, &moved).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(saved.set("not-durable", "staged", None).is_err());
        assert!(!saved.entries.contains_key("not-durable"));
        drop(saved);
        std::fs::remove_dir_all(root).unwrap();
    }
}
pub struct Journal {
    pub entries: BTreeMap<String, Receipt>,
    path: PathBuf,
}
pub struct Handoffs(pub Mutex<Journal>);
impl Handoffs {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let mut entries: BTreeMap<String, Receipt> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid handoff journal: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.to_string()),
        };
        // A previous process cannot finish validation. Existing staged records are
        // reconciled from the durable catalog before this negative receipt is used.
        for receipt in entries.values_mut() {
            if receipt.state == "pending" {
                receipt.state = "interrupted".into();
            }
        }
        let journal = Journal { entries, path };
        journal.persist()?;
        Ok(Self(Mutex::new(journal)))
    }
}
impl Journal {
    fn persist(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(&self.entries).map_err(|e| e.to_string())?;
        crate::platform::write_private_atomic(&self.path, &bytes).map_err(|e| e.to_string())
    }
    pub fn set(&mut self, id: &str, state: &str, digest: Option<String>) -> Result<(), String> {
        let old = self.entries.clone();
        let now = chrono::Utc::now().timestamp();
        self.entries.retain(|_, r| {
            !matches!(r.state.as_str(), "aborted" | "rejected" | "committed")
                || now - r.updated < 7 * 86400
        });
        if !self.entries.contains_key(id) && self.entries.len() >= 4096 {
            return Err("Handoff receipt capacity reached. Finish pending transfers first.".into());
        }
        let digest = digest.or_else(|| self.entries.get(id).and_then(|r| r.digest.clone()));
        let ids = self
            .entries
            .get(id)
            .map(|r| r.ids.clone())
            .unwrap_or_default();
        self.entries.insert(
            id.into(),
            Receipt {
                state: state.into(),
                digest,
                updated: now,
                ids,
            },
        );
        if let Err(e) = self.persist() {
            self.entries = old;
            return Err(e);
        }
        Ok(())
    }
    pub fn result(&self, id: &str) -> Value {
        let receipt = self.entries.get(id);
        json!({"state":receipt.map(|r| if r.state == "interrupted" { "aborted" } else { r.state.as_str() }).unwrap_or("unknown"), "ids":receipt.map(|r|r.ids.clone()).unwrap_or_default(), "statuses":[], "committed":receipt.is_some_and(|r|r.state == "committed")})
    }
    pub fn commit(&mut self, id: &str, ids: Vec<String>) -> Result<(), String> {
        if self
            .entries
            .get(id)
            .is_some_and(|r| r.state == "committed" && r.ids == ids)
        {
            return Ok(());
        }
        let old = self.entries.clone();
        let digest = self.entries.get(id).and_then(|r| r.digest.clone());
        self.entries.insert(
            id.into(),
            Receipt {
                state: "committed".into(),
                digest,
                ids,
                updated: chrono::Utc::now().timestamp(),
            },
        );
        if let Err(error) = self.persist() {
            self.entries = old;
            return Err(error);
        }
        Ok(())
    }
}
