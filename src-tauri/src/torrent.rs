//! The sole owner of the C++ engine lives on a dedicated thread. No native call
//! or alert/disk wait runs on the UI or Tokio executor threads.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    ffi::{c_char, c_void, CStr, CString},
    path::Path,
    sync::mpsc,
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

type Request = (Value, oneshot::Sender<Result<Value, String>>);
pub struct TorrentEngine {
    sender: mpsc::SyncSender<Request>,
}
impl TorrentEngine {
    pub fn new(root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let root = root
            .to_str()
            .ok_or("Torrent state requires a UTF-8 path; choose a UTF-8 home/data directory.")?
            .to_owned();
        let (sender, receiver) = mpsc::sync_channel::<Request>(128);
        std::thread::Builder::new()
            .name("torrent-control".into())
            .spawn(move || {
                let native = Native(unsafe { fr_torrent_new() });
                if native.0.is_null() {
                    return;
                }
                let _ = native.call(json!({"op":"init", "root":root}));
                while let Ok((request, reply)) = receiver.recv() {
                    let _ = reply.send(native.call(request));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { sender })
    }
    pub async fn call(&self, request: Value) -> Result<Value, String> {
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send((request, reply))
            .map_err(|e| format!("Torrent control queue unavailable: {e}"))?;
        result
            .await
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
