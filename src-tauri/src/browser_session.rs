use crate::model::AddDownloadRequest;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

const LIFETIME: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Pending {
    id: Uuid,
    created: i64,
    consumer: Option<Uuid>,
    claimed_at: i64,
    batch: Option<Uuid>,
    completed: bool,
    requests: Vec<AddDownloadRequest>,
}

/// Desktop links wait here until the user chooses a signed-in browser window.
/// The protected app journal retains claims across app restarts for fifteen minutes.
#[derive(Default)]
pub struct Requests(Mutex<Vec<Pending>>, Option<PathBuf>);

impl Requests {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let pending = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("Invalid browser request journal: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self(Mutex::new(pending), Some(path)))
    }
    fn persist(&self, pending: &[Pending]) -> Result<(), String> {
        if let Some(path) = &self.1 {
            crate::platform::write_private_atomic(
                path,
                &serde_json::to_vec(pending).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    pub fn enqueue(&self, requests: Vec<AddDownloadRequest>) -> Result<Uuid, String> {
        if requests.is_empty() || requests.len() > 1000 {
            return Err("Choose between 1 and 1,000 HTTP download links.".into());
        }
        for request in &requests {
            let url = url::Url::parse(&request.url).map_err(|_| "Invalid download URL.")?;
            if !matches!(url.scheme(), "http" | "https") || request.url.len() > 8192 {
                return Err("Browser sessions support HTTP and HTTPS links only.".into());
            }
            if request.request_context.is_some() {
                return Err("Choose the browser session in the companion.".into());
            }
        }
        if serde_json::to_vec(&requests)
            .map_err(|e| e.to_string())?
            .len()
            > 256 * 1024 - 4096
        {
            return Err(
                "These browser download links are too large. Submit a smaller batch.".into(),
            );
        }
        let mut pending = self.0.lock().map_err(|e| e.to_string())?;
        pending.retain(|item| {
            chrono::Utc::now().timestamp().saturating_sub(item.created) < LIFETIME.as_secs() as i64
        });
        if pending.iter().filter(|item| !item.completed).count() >= 10 || pending.len() >= 500 {
            return Err("Finish the pending downloads in your browser companion first.".into());
        }
        let id = Uuid::new_v4();
        let old = pending.clone();
        pending.push(Pending {
            id,
            created: chrono::Utc::now().timestamp(),
            consumer: None,
            claimed_at: chrono::Utc::now().timestamp(),
            batch: None,
            completed: false,
            requests,
        });
        if let Err(error) = self.persist(&pending) {
            *pending = old;
            return Err(error);
        }
        Ok(id)
    }

    pub fn summaries(&self) -> Vec<Value> {
        let Ok(mut pending) = self.0.lock() else {
            return Vec::new();
        };
        pending.retain(|item| {
            chrono::Utc::now().timestamp().saturating_sub(item.created) < LIFETIME.as_secs() as i64
        });
        pending
            .iter()
            .filter(|item| !item.completed)
            .map(|item| json!({"id": item.id, "count": item.requests.len(), "claimed":item.consumer.is_some(), "leaseSeconds":120}))
            .collect()
    }

    pub fn take(&self, id: Uuid, consumer: Uuid) -> Result<Vec<AddDownloadRequest>, String> {
        let mut pending = self.0.lock().map_err(|e| e.to_string())?;
        pending.retain(|item| {
            chrono::Utc::now().timestamp().saturating_sub(item.created) < LIFETIME.as_secs() as i64
        });
        let old = pending.clone();
        let item = pending.iter_mut().find(|item| item.id == id && !item.completed)
            .ok_or("This browser request expired or is already being handled. Submit the links again in Fetchrail.")?;
        if item.consumer.is_some_and(|owner| owner != consumer)
            && (item.batch.is_some()
                || chrono::Utc::now()
                    .timestamp()
                    .saturating_sub(item.claimed_at)
                    < 120)
        {
            return Err(
                "Another browser window holds this request. Wait for its lease or resume it there."
                    .into(),
            );
        }
        item.consumer = Some(consumer);
        item.claimed_at = chrono::Utc::now().timestamp();
        let requests = item.requests.clone();
        if let Err(error) = self.persist(&pending) {
            *pending = old;
            return Err(error);
        }
        Ok(requests)
    }

    pub fn finish(
        &self,
        id: Uuid,
        consumer: Uuid,
        batch: Uuid,
        completed: bool,
    ) -> Result<(), String> {
        let mut pending = self.0.lock().map_err(|e| e.to_string())?;
        let old = pending.clone();
        let index = pending
            .iter()
            .position(|item| item.id == id && item.consumer == Some(consumer))
            .ok_or("This browser request is no longer available.")?;
        if completed {
            if pending[index].batch.is_some_and(|id| id != batch) {
                return Err("Browser request belongs to another batch.".into());
            }
            pending[index].batch = Some(batch);
            pending[index].completed = true;
        } else {
            if pending[index].completed {
                return Err("This request was already bound to a batch.".into());
            }
            pending[index].consumer = None;
        }
        if let Err(error) = self.persist(&pending) {
            *pending = old;
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> AddDownloadRequest {
        serde_json::from_value(
            json!({"url":"https://github.com/private/repo/archive/main.zip",
            "directory":"C:/Downloads", "queue":"Night", "startPaused":true,
            "connections":4, "speedLimitBps":1024, "expectedSha256":"a".repeat(64)}),
        )
        .unwrap()
    }

    #[test]
    fn browser_selection_preserves_options_and_claims_once() {
        let pending = Requests::default();
        let consumer = Uuid::new_v4();
        let batch = Uuid::new_v4();
        let id = pending.enqueue(vec![request()]).unwrap();
        assert_eq!(
            pending.summaries(),
            vec![json!({"id": id, "count":1, "claimed":false, "leaseSeconds":120})]
        );
        assert!(!pending.summaries()[0].to_string().contains("github"));
        let requests = pending.take(id, consumer).unwrap();
        assert_eq!(requests[0].directory.as_deref(), Some("C:/Downloads"));
        assert_eq!(requests[0].speed_limit_bps, Some(1024));
        assert_eq!(
            requests[0].expected_sha256.as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert!(pending.take(id, consumer).is_ok());
        assert!(pending.take(id, Uuid::new_v4()).is_err());
        assert_eq!(pending.summaries().len(), 1);
        pending.finish(id, consumer, batch, false).unwrap();
        assert!(pending.take(id, consumer).is_ok());
        pending.finish(id, consumer, batch, true).unwrap();
        assert!(pending.take(id, consumer).is_err());
    }

    #[test]
    fn invalid_expired_and_oversized_requests_are_rejected() {
        let pending = Requests::default();
        let consumer = Uuid::new_v4();
        assert!(pending.enqueue(vec![]).is_err());
        let mut invalid = request();
        invalid.url = "file:///private".into();
        assert!(pending.enqueue(vec![invalid]).is_err());
        let mut secret = request();
        secret.request_context = Some(Default::default());
        assert!(pending.enqueue(vec![secret]).is_err());
        let id = pending.enqueue(vec![request()]).unwrap();
        pending.0.lock().unwrap()[0].created =
            chrono::Utc::now().timestamp() - LIFETIME.as_secs() as i64;
        assert!(pending.take(id, consumer).is_err());
        for _ in 0..10 {
            pending.enqueue(vec![request()]).unwrap();
        }
        assert!(pending.enqueue(vec![request()]).is_err());
    }

    #[test]
    fn claim_lease_and_terminal_batch_receipt_are_idempotent_across_restart() {
        let root = std::env::temp_dir().join(format!("fetchrail-claim-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("requests.json");
        let queue = Requests::load(path.clone()).unwrap();
        let id = queue.enqueue(vec![request()]).unwrap();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let batch = Uuid::new_v4();
        queue.take(id, first).unwrap();
        let restarted = Requests::load(path).unwrap();
        assert!(
            restarted.take(id, first).is_ok(),
            "A lost take reply can be retried by the same consumer"
        );
        assert!(restarted.take(id, second).is_err());
        restarted.0.lock().unwrap()[0].claimed_at -= 121;
        assert!(
            restarted.take(id, second).is_ok(),
            "Unbound expired claims are released"
        );
        assert!(restarted.finish(id, first, batch, true).is_err());
        restarted.finish(id, second, batch, true).unwrap();
        restarted.finish(id, second, batch, true).unwrap();
        assert!(restarted.finish(id, second, Uuid::new_v4(), true).is_err());
        assert!(restarted.finish(id, second, batch, false).is_err());
        assert!(restarted.summaries().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
