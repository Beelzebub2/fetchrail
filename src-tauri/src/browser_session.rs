use crate::model::AddDownloadRequest;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

const LIFETIME: Duration = Duration::from_secs(15 * 60);

struct Pending {
    id: Uuid,
    created: Instant,
    claimed: bool,
    requests: Vec<AddDownloadRequest>,
}

/// Desktop links wait here until the user chooses a signed-in browser window.
/// No browser profile is guessed, and nothing is written to disk.
#[derive(Default)]
pub struct Requests(Mutex<Vec<Pending>>);

impl Requests {
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
            > 256 * 1024
        {
            return Err(
                "These browser download links are too large. Submit a smaller batch.".into(),
            );
        }
        let mut pending = self.0.lock().map_err(|e| e.to_string())?;
        pending.retain(|item| item.created.elapsed() < LIFETIME);
        if pending.len() >= 10 {
            return Err("Finish the pending downloads in your browser companion first.".into());
        }
        let id = Uuid::new_v4();
        pending.push(Pending {
            id,
            created: Instant::now(),
            claimed: false,
            requests,
        });
        Ok(id)
    }

    pub fn summaries(&self) -> Vec<Value> {
        let Ok(mut pending) = self.0.lock() else {
            return Vec::new();
        };
        pending.retain(|item| item.created.elapsed() < LIFETIME);
        pending
            .iter()
            .filter(|item| !item.claimed)
            .map(|item| json!({"id": item.id, "count": item.requests.len()}))
            .collect()
    }

    pub fn take(&self, id: Uuid) -> Result<Vec<AddDownloadRequest>, String> {
        let mut pending = self.0.lock().map_err(|e| e.to_string())?;
        pending.retain(|item| item.created.elapsed() < LIFETIME);
        let item = pending.iter_mut().find(|item| item.id == id && !item.claimed)
            .ok_or("This browser request expired or is already being handled. Submit the links again in Fetchrail.")?;
        item.claimed = true;
        Ok(item.requests.clone())
    }

    pub fn finish(&self, id: Uuid, completed: bool) -> Result<(), String> {
        let mut pending = self.0.lock().map_err(|e| e.to_string())?;
        let index = pending
            .iter()
            .position(|item| item.id == id && item.claimed)
            .ok_or("This browser request is no longer available.")?;
        if completed {
            pending.remove(index);
        } else {
            pending[index].claimed = false;
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
        let id = pending.enqueue(vec![request()]).unwrap();
        assert_eq!(pending.summaries(), vec![json!({"id": id, "count":1})]);
        assert!(!pending.summaries()[0].to_string().contains("github"));
        let requests = pending.take(id).unwrap();
        assert_eq!(requests[0].directory.as_deref(), Some("C:/Downloads"));
        assert_eq!(requests[0].speed_limit_bps, Some(1024));
        assert_eq!(
            requests[0].expected_sha256.as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert!(pending.take(id).is_err());
        assert!(pending.summaries().is_empty());
        pending.finish(id, false).unwrap();
        assert!(pending.take(id).is_ok());
        pending.finish(id, true).unwrap();
        assert!(pending.take(id).is_err());
    }

    #[test]
    fn invalid_expired_and_oversized_requests_are_rejected() {
        let pending = Requests::default();
        assert!(pending.enqueue(vec![]).is_err());
        let mut invalid = request();
        invalid.url = "file:///private".into();
        assert!(pending.enqueue(vec![invalid]).is_err());
        let mut secret = request();
        secret.request_context = Some(Default::default());
        assert!(pending.enqueue(vec![secret]).is_err());
        let id = pending.enqueue(vec![request()]).unwrap();
        pending.0.lock().unwrap()[0].created = Instant::now() - LIFETIME;
        assert!(pending.take(id).is_err());
        for _ in 0..10 {
            pending.enqueue(vec![request()]).unwrap();
        }
        assert!(pending.enqueue(vec![request()]).is_err());
    }
}
