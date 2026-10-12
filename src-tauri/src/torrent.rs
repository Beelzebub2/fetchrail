//! The sole owner of the C++ engine lives on a dedicated thread. No native call
//! or alert/disk wait runs on the UI or Tokio executor threads.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    ffi::{c_char, c_void, CStr, CString},
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

extern "C" {
    fn fr_torrent_new() -> *mut c_void;
    fn fr_torrent_drop(ptr: *mut c_void);
    fn fr_torrent_call(ptr: *mut c_void, input: *const c_char) -> *mut c_char;
    fn fr_torrent_free(ptr: *mut c_char);
}

struct Native(*mut c_void);
impl Native {
    fn call(&self, request: Value) -> Result<Value, String> {
        let input = CString::new(request.to_string()).map_err(|e| e.to_string())?;
        // Both pointers stay owned by this thread, and C++ catches every exception.
        let result = unsafe { fr_torrent_call(self.0, input.as_ptr()) };
        if result.is_null() {
            return Err("Torrent engine ran out of memory".into());
        }
        let bytes = unsafe { CStr::from_ptr(result).to_bytes().to_vec() };
        unsafe { fr_torrent_free(result) };
        let envelope: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if let Some(error) = envelope.get("error") {
            return Err(error.as_str().unwrap_or("Torrent engine failed").into());
        }
        Ok(envelope["ok"].clone())
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        unsafe { fr_torrent_drop(self.0) };
    }
}

type Request = (Value, Instant, oneshot::Sender<Result<Value, String>>);
pub struct TorrentEngine {
    sender: mpsc::SyncSender<Request>,
    pub recovered_unclean: bool,
}
impl TorrentEngine {
    pub fn new(root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let root = root
            .to_str()
            .ok_or("Torrent state requires a UTF-8 path; choose a UTF-8 home/data directory.")?
            .to_owned();
        let (sender, receiver) = mpsc::sync_channel::<Request>(128);
        let (ready, initialized) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("torrent-control".into())
            .spawn(move || {
                let native = Native(unsafe { fr_torrent_new() });
                if native.0.is_null() {
                    let _ =
                        ready.send(Err("Could not initialize native torrent engine".to_string()));
                    return;
                }
                let result = native
                    .call(json!({"op":"init", "root":root}))
                    .map(|value| value["unclean"].as_bool().unwrap_or(true));
                let failed = result.is_err();
                let _ = ready.send(result);
                if failed {
                    return;
                }
                while let Ok((request, deadline, reply)) = receiver.recv() {
                    // Dropped/expired queued mutations must never run after the caller timed out.
                    if reply.is_closed() || Instant::now() >= deadline {
                        continue;
                    }
                    let _ = reply.send(native.call(request));
                }
            })
            .map_err(|e| e.to_string())?;
        let recovered_unclean = initialized
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "Torrent initialization timed out".to_string())??;
        Ok(Self {
            sender,
            recovered_unclean,
        })
    }
    pub async fn call(&self, request: Value) -> Result<Value, String> {
        let (reply, result) = oneshot::channel();
        let timeout = if request["op"] == "poll" || request["op"] == "details" {
            Duration::from_secs(3)
        } else if request["op"] == "checkpoint" {
            Duration::from_secs(35)
        } else {
            Duration::from_secs(20)
        };
        self.sender
            .try_send((request, Instant::now() + timeout, reply))
            .map_err(|e| format!("Torrent control queue unavailable: {e}"))?;
        tokio::time::timeout(timeout, result)
            .await.map_err(|_| "Torrent command timed out; its in-flight outcome is uncertain. Refresh before retrying a storage mutation.".to_string())?
            .map_err(|_| "Torrent worker stopped".to_string())?
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentFile {
    pub index: usize,
    pub path: String,
    pub size: u64,
    pub priority: u8,
    pub downloaded: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentMetadata {
    pub file_count: usize,
    pub name: String,
    pub hashes: Vec<String>,
    pub private: bool,
    pub total_bytes: u64,
    pub files: Vec<TorrentFile>,
    pub piece_length: u64,
    pub pieces: usize,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct TorrentSummary {
    pub hashes: Vec<String>,
    pub uploaded_bytes: u64,
    pub all_downloaded_bytes: u64,
    pub upload_speed_bps: u64,
    pub peers: usize,
    pub seeds: usize,
    pub selected_ready: bool,
    pub active_seconds: u64,
    pub seed_seconds: u64,
    pub ratio_limit: f64,
    pub seed_time_limit: u64,
    pub sequential: bool,
    #[serde(skip_serializing)]
    pub priorities: std::sync::Arc<Vec<u8>>,
    pub metadata_path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TorrentSettings {
    pub upload_limit_bps: u64,
    pub max_seeds: usize,
    pub ratio_limit: f64,
    pub seed_time_limit: u64,
    pub dht: bool,
    pub lsd: bool,
    pub upnp: bool,
    pub connections: usize,
    pub listen_interfaces: String,
    pub outgoing_interfaces: String,
    pub encryption: u8,
    pub protocol: u8,
    pub proxy_type: u8,
    pub proxy_host: String,
    pub proxy_port: u16,
    pub proxy_username: String,
    pub proxy_password: String,
}
impl Default for TorrentSettings {
    fn default() -> Self {
        Self {
            upload_limit_bps: 0,
            max_seeds: 5,
            ratio_limit: 1.0,
            seed_time_limit: 86400,
            dht: true,
            lsd: true,
            upnp: true,
            connections: 500,
            listen_interfaces: "0.0.0.0:0,[::]:0".into(),
            outgoing_interfaces: String::new(),
            encryption: 1,
            protocol: 0,
            proxy_type: 0,
            proxy_host: String::new(),
            proxy_port: 0,
            proxy_username: String::new(),
            proxy_password: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentImportRequest {
    pub source: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TorrentCommitRequest {
    pub id: uuid::Uuid,
    pub directory: String,
    pub priorities: Vec<u8>,
    pub queue: String,
    pub start_paused: bool,
    #[serde(default)]
    pub verify_existing: bool,
    pub scheduled_for: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[tokio::test]
    async fn startup_handshake_surfaces_native_init_failure_and_absent_removal_is_safe() {
        let root =
            std::env::temp_dir().join(format!("fetchrail-native-init-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("unclean")).unwrap();
        assert!(TorrentEngine::new(&root).is_err());
        std::fs::remove_dir(root.join("unclean")).unwrap();
        let engine = TorrentEngine::new(&root).unwrap();
        engine
            .call(json!({"op":"remove", "id":uuid::Uuid::new_v4()}))
            .await
            .unwrap();
        engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap();
        engine.call(json!({"op":"clean"})).await.unwrap();
        drop(engine);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn native_checkpoint_failure_preserves_old_resume_then_explicit_retry_succeeds() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::temp_dir().join(format!(
            "fetchrail-native-checkpoint-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let payload = root.join("payload.bin");
        std::fs::write(&payload, vec![0x5a; 32768]).unwrap();
        let metadata = root.join("fixture.torrent");
        let engine = TorrentEngine::new(&root.join("state")).unwrap();
        engine.call(json!({"op":"create", "path":payload, "output":metadata, "format":"v1", "pieceLength":16384})).await.unwrap();
        let id = uuid::Uuid::new_v4();
        engine
            .call(
                json!({"op":"add", "id":id, "source":metadata, "destination":root,
            "config":{"dht":false,"lsd":false,"upnp":false,"listenInterfaces":"127.0.0.1:0"}}),
            )
            .await
            .unwrap();
        engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap();
        let resume = root.join("state").join(format!("{id}.resume"));
        let previous = std::fs::read(&resume).unwrap();
        // Deny replacement of the known-good resume while the real native save runs.
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&resume)
            .unwrap();
        let error = engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap_err();
        assert!(error.contains("checkpoint"), "{error}");
        drop(locked);
        assert_eq!(std::fs::read(&resume).unwrap(), previous);
        engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap();
        engine
            .call(json!({"op":"tracker", "id":id, "url":"http://127.0.0.1:1/announce"}))
            .await
            .unwrap();
        engine
            .call(json!({"op":"checkpoint", "wait":true}))
            .await
            .unwrap();
        let tracker = b"http://127.0.0.1:1/announce";
        assert!(std::fs::read(&resume)
            .unwrap()
            .windows(tracker.len())
            .any(|w| w == tracker));
        std::fs::remove_file(&resume).unwrap();
        engine.call(json!({"op":"checkpoint"})).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !resume.exists() {
                engine.call(json!({"op":"poll"})).await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect(
            "A missing known-good checkpoint must be recreated, never accepted as not-modified",
        );
        engine.call(json!({"op":"remove", "id":id})).await.unwrap();
        engine.call(json!({"op":"clean"})).await.unwrap();
        drop(engine);
        std::fs::remove_dir_all(root).unwrap();
    }
}
